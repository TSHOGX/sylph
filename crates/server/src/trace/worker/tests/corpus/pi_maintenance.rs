use super::*;
use sha2::{Digest, Sha256};

const RETRY: Corpus = Corpus {
    vendor: "pi",
    session: "01a0c7e8-834c-7671-b01a-880a146f815a",
    native: include_bytes!("../../../../../../../fixtures/unified-trace/pi/native-retry.jsonl"),
    live: include_bytes!("../../../../../../../fixtures/unified-trace/pi/stdout-retry.jsonl"),
    namespace: "pi.entry.message",
    question_id: "44565221",
    answer_id: "5ebd81b8",
    question: b"Reply with exactly SYLPH_PI_AA_OK.",
    answer: b"SYLPH_PI_AA_OK",
};
const COMPACTION: Corpus = Corpus {
    vendor: "pi",
    session: "01a0c7e8-85fb-73b4-9503-ee8990853edc",
    native: include_bytes!(
        "../../../../../../../fixtures/unified-trace/pi/native-compaction.jsonl"
    ),
    live: include_bytes!("../../../../../../../fixtures/unified-trace/pi/stdout-compaction.jsonl"),
    namespace: "pi.entry.message",
    question_id: "d62d0b29",
    answer_id: "38e2a001",
    question: b"Reply with exactly SYLPH_PI_AA_OK.",
    answer: b"SYLPH_PI_AA_OK",
};

async fn expected(corpus: &Corpus, fixture: &Fixture, store: &GenerationStore, retry: bool) {
    let retained = claims(store).await;
    let reduced = reduction(&retained);
    let activations: Vec<_> = reduced
        .executions
        .iter()
        .filter(|(reference, _)| matches!(reference, AnyObjectRef::Activation(_)))
        .collect();
    assert_eq!(activations.len(), 1);
    assert_eq!(activations[0].0.scope(), &corpus.owner());
    assert_eq!(
        activations[0].1.outcome.known(),
        Some(&TerminalOutcome::Succeeded)
    );
    let operations: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|value| match value.declaration.known() {
            Some(TraceObject::Operation(operation)) => Some(operation),
            _ => None,
        })
        .collect();
    let inferences: Vec<_> = operations
        .iter()
        .filter(|operation| matches!(operation.category, OperationCategory::Inference { .. }))
        .collect();
    assert_eq!(inferences.len(), if retry { 3 } else { 1 });
    let failed = inferences
        .iter()
        .filter(|operation| {
            reduced
                .execution(&AnyObjectRef::Operation(operation.operation_ref.clone()))
                .unwrap()
                .outcome
                .known()
                == Some(&TerminalOutcome::Failed)
        })
        .count();
    assert_eq!(failed, if retry { 2 } else { 0 });
    let name = if retry { "retry" } else { "compaction" };
    let work: Vec<_> = operations.iter().filter(|operation|matches!(&operation.category,OperationCategory::Extension{namespace,name: actual} if namespace=="pi.runtime" && actual==name)).collect();
    assert_eq!(work.len(), 1);
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Operation(work[0].operation_ref.clone()))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    assert_eq!(work[0].activation.is_some(), retry);
    let outcomes: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|value| match value.declaration.known() {
            Some(TraceObject::Result(result)) if result.operation == work[0].operation_ref => {
                Some(result)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| result.disposition == ResultDisposition::Progress)
            .count(),
        if retry { 2 } else { 0 }
    );
    let final_result = outcomes
        .iter()
        .find(|result| result.disposition == ResultDisposition::FinalReturn)
        .unwrap();
    let bytes = fixture
        .evidence
        .payload_bytes(&final_result.payload.as_ref().unwrap().reference, 64 * 1024)
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    if retry {
        assert_eq!(result["success"], true);
        assert_eq!(result["attempt"], 2);
        assert!(retained.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic} if diagnostic.code==DiagnosticCode::ReportedFailure && diagnostic.message.contains("Controlled overloaded provider attempt"))));
    } else {
        assert_eq!(result["reason"], "manual");
        assert_eq!(result["result"]["firstKeptEntryId"], "38e2a001");
        assert_eq!(result["result"]["summary"],"No prior history.\n\n---\n\n**Turn Context (split turn):**\n\nSYLPH_PI_COMPACTION_SUMMARY");
        let checkpoint =
            CheckpointRef::native(corpus.owner(), "pi.checkpoint", vec!["a7cd56da".into()])
                .unwrap();
        assert!(reduced
            .object(&AnyObjectRef::ContextCheckpoint(checkpoint))
            .is_some());
    }
    assert!(
        !retained
            .iter()
            .any(|claim| matches!(claim.fact, Fact::Equivalence { .. })),
        "no text-based live/native certificate was captured"
    );
    for (id, body) in [
        (corpus.question_id, corpus.question),
        (corpus.answer_id, corpus.answer),
    ] {
        let bodies = revision_bytes(fixture, store, &corpus.message(id)).await;
        assert!(!bodies.is_empty());
        assert!(bodies.iter().all(|bytes| bytes == body));
    }
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let page = store
        .read_page(
            &snapshot,
            &read_scope(&corpus.owner(), None, "activations"),
            None,
            None,
            None,
            8,
            false,
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    let summary: ExecutionSummary = serde_json::from_value(page.rows[0].body.clone()).unwrap();
    assert!(matches!(
        summary.state,
        ReadExecutionState::Terminated {
            outcome: TerminalOutcome::Succeeded,
            ..
        }
    ));
    assert!(
        summary.durations.is_empty(),
        "receipt timestamps cannot fabricate native elapsed time"
    );
}

async fn replay_case(
    corpus: &Corpus,
    order: SourceOrder,
    incremental: bool,
    retry: bool,
) -> Projection {
    let fixture = Fixture::for_vendor("pi").await;
    let (mut store, _) = fixture.generation("pi-maintenance").await;
    for (index, record) in records(corpus, order).iter().enumerate() {
        let captured = admit_record(&fixture, record).await;
        fixture
            .evidence
            .admit_batch(
                &captured.observation_ref.issuer,
                std::slice::from_ref(&captured),
            )
            .await
            .unwrap();
        let is_retry_end = record.source == "corpus-live"
            && serde_json::from_slice::<serde_json::Value>(record.raw).unwrap()["type"]
                == "auto_retry_end";
        if incremental || is_retry_end {
            fixture
                .worker_with(WorkerLimits {
                    max_unit_observations: 1 + (index % 4),
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            if is_retry_end {
                let reduced = reduction(&claims(&store).await);
                let outer: Vec<_> = reduced
                    .executions
                    .iter()
                    .filter(|(reference, _)| matches!(reference, AnyObjectRef::Activation(_)))
                    .collect();
                assert_eq!(outer.len(), 1);
                assert!(outer[0].1.terminated.is_empty());
                assert!(outer[0].1.outcome.known().is_none());
            }
            if index % 5 == 0 {
                store = reopened(store).await;
            }
        }
    }
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    expected(corpus, &fixture, &store, retry).await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("pi-maintenance-rebuilt").await;
    fixture.worker().catch_up(&fresh, 1, cut).await.unwrap();
    expected(corpus, &fixture, &fresh, retry).await;
    assert_projection(&projection(&fixture, &fresh).await, &before);
    Projection {
        claims: before.claims,
        reader: before
            .reader
            .normalize_separate_archive_epochs(&fixture.catalogue)
            .await,
    }
}

#[tokio::test]
async fn retained_pi_retry_and_compaction_preserve_own_outcomes_across_source_orders_and_restart() {
    for (corpus, retry, native_digest, live_digest) in [
        (
            &RETRY,
            true,
            "6314d0240bfa95034e12bb91fa20a29d44926801ab6d1a9c0643238601cd9040",
            "eca2587ef6817843663d86816a77a7d5b6466e73e6b48a69ce459e96baacd20a",
        ),
        (
            &COMPACTION,
            false,
            "2cdb10ae277a3ec40c0d8765ed14db6c95b16f18f25a1749b491debf8d53ccab",
            "f935c0cecdd4c1680261bb07aaebbf8f356ce4a2422fad5b62df93def7d11338",
        ),
    ] {
        assert_eq!(
            format!("{:x}", Sha256::digest(corpus.native)),
            native_digest
        );
        assert_eq!(format!("{:x}", Sha256::digest(corpus.live)), live_digest);
        let cold = replay_case(corpus, SourceOrder::NativeFirst, false, retry).await;
        assert_projection(
            &replay_case(corpus, SourceOrder::LiveFirst, true, retry).await,
            &cold,
        );
        assert_projection(
            &replay_case(corpus, SourceOrder::Alternating, true, retry).await,
            &cold,
        );
    }
}
