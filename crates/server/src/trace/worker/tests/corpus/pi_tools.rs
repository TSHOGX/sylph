use super::*;
use sha2::{Digest, Sha256};
use sylph_protocol::SourceAttachmentEvent;

const CORPUS: Corpus = Corpus {
    vendor: "pi",
    session: "01a0c934-6456-70d6-a80d-e75aef121202",
    native: include_bytes!(
        "../../../../../../../fixtures/unified-trace/pi/tool-sidecar/native.jsonl"
    ),
    live: include_bytes!(
        "../../../../../../../fixtures/unified-trace/pi/tool-sidecar/stdout.jsonl"
    ),
    namespace: "pi.entry.message",
    question_id: "ea5257a8",
    answer_id: "053cf86b",
    question: b"Reply with exactly SYLPH_PI_AA_OK.",
    answer: b"SYLPH_PI_AA_OK",
};
const BODY: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/pi/tool-sidecar/sidecar.txt");
fn observation(record: &Record) -> ObservationRef {
    ObservationRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
        producer_id: record.source.into(),
        producer_epoch: "retained-capture".into(),
        producer_seq: record.ordinal + 1,
    }
}
async fn attachments(fixture: &Fixture) -> (PayloadRef, Vec<Capture>) {
    let body = fixture.stage(BODY).await;
    let mut captures = vec![];
    for record in records(&CORPUS, SourceOrder::NativeFirst) {
        let kind = if record.source == "corpus-native" {
            TransportKind::NativeJsonl
        } else {
            TransportKind::LiveStdout
        };
        for reference in sylph_adapters::native::inventory::sidecar_references(record.raw, kind) {
            let issuer = observation(&record).issuer;
            let suffix = if reference.locator.ends_with("sidecar-1.txt") {
                1
            } else {
                assert!(reference.locator.ends_with("sidecar-2.txt"));
                2
            };
            let source_position = SourcePosition {
                source: SourceRef {
                    issuer: issuer.clone(),
                    instance: format!("pi-output-{suffix}"),
                },
                generation: 0,
                locator: SourceLocator::Snapshot {
                    revision: "0".into(),
                },
            };
            let attachment = CaptureAttachment {
                locator: reference.locator,
                source_position: Some(source_position),
                observed_version: Some("0".into()),
                payload_ref: Some(body.clone()),
                availability: AttachmentAvailability::Captured,
                relation: reference.relation,
            };
            let event = SourceAttachmentEvent {
                parent_observation: observation(&record),
                parent_slot: reference.parent_slot,
                attachment: attachment.clone(),
            };
            let name = format!("pi-sidecar-binding-{}", captures.len());
            captures.push(Capture {
                observation_ref: ObservationRef {
                    issuer: issuer.clone(),
                    producer_id: name.clone(),
                    producer_epoch: "retained-capture".into(),
                    producer_seq: 1,
                },
                source_position: SourcePosition {
                    source: SourceRef {
                        issuer,
                        instance: name,
                    },
                    generation: 0,
                    locator: SourceLocator::Frame { ordinal: 0 },
                },
                ingestion_authorization_epoch: fixture.epoch.clone(),
                declared_subject_refs: vec![],
                captured_at: TimeEvidence {
                    domain: "fixture.wall".into(),
                    value: 100,
                    unit: TimeUnit::Milliseconds,
                    precision: Some(1),
                    basis: TimeBasis::SourceWall,
                },
                source_clock: None,
                transport_kind: TransportKind::SourceAttachment,
                vendor_build: Some("0.85.1".into()),
                schema_fingerprint: None,
                payload_ref: fixture.stage(&serde_json::to_vec(&event).unwrap()).await,
                attachments: vec![attachment],
                capture_diagnostics: vec![],
            });
        }
    }
    assert!(
        captures.len() > 10,
        "the corpus includes progress, message, end and entry-window references"
    );
    (body, captures)
}
async fn expected(fixture: &Fixture, store: &GenerationStore, body: &PayloadRef) {
    let retained = claims(store).await;
    let reduced = reduction(&retained);
    let tools:Vec<_>=reduced.objects.values().filter_map(|object|match object.declaration.known(){Some(TraceObject::Operation(operation))if matches!(&operation.category,OperationCategory::Tool{namespace:Some(namespace),name}if namespace=="pi"&&name=="bash")=>Some(operation),_=>None}).collect();
    assert_eq!(
        tools.len(),
        4,
        "two native descriptions and two independently identified live invocations"
    );
    let live: Vec<_> = tools
        .iter()
        .filter(|op| matches!(op.operation_ref.address(), ObjectAddress::Occurrence { .. }))
        .collect();
    assert_eq!(live.len(), 2);
    for operation in live {
        let execution = reduced
            .execution(&AnyObjectRef::Operation(operation.operation_ref.clone()))
            .unwrap();
        assert_eq!(execution.outcome.known(), Some(&TerminalOutcome::Succeeded));
        assert!(!execution.started.is_empty());
        assert!(reduced.objects.values().any(|object|matches!(object.declaration.known(),Some(TraceObject::Result(result))if result.operation==operation.operation_ref&&result.representation==Representation::CallerResult)));
    }
    let sidecars: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Result(result)) if result.payload.as_ref() == Some(body) => {
                Some(result)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        sidecars.len(),
        4,
        "shared bytes do not collapse unjoined native/live invocations"
    );
    assert_eq!(
        sidecars
            .iter()
            .map(|result| result.operation.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );
    assert_eq!(
        fixture
            .evidence
            .payload_bytes(body, BODY.len())
            .await
            .unwrap(),
        BODY
    );
    for (id, expected) in [
        (CORPUS.question_id, CORPUS.question),
        (CORPUS.answer_id, CORPUS.answer),
    ] {
        let bytes = revision_bytes(fixture, store, &CORPUS.message(id)).await;
        assert!(!bytes.is_empty());
        assert!(bytes.iter().all(|bytes| bytes == expected));
    }
    assert!(
        !retained
            .iter()
            .any(|claim| matches!(claim.fact, Fact::Equivalence { .. })),
        "the capture supplies no native/live message certificate"
    );
    let activations: Vec<_> = reduced
        .executions
        .iter()
        .filter(|(reference, _)| matches!(reference, AnyObjectRef::Activation(_)))
        .collect();
    assert_eq!(activations.len(), 1);
    assert_eq!(
        activations[0].1.outcome.known(),
        Some(&TerminalOutcome::Succeeded)
    );
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    for operation in tools {
        let reference = AnyObjectRef::Operation(operation.operation_ref.clone());
        let row = store
            .read_row(
                &snapshot,
                &read_scope(&CORPUS.owner(), None, "objects"),
                &reference.encode(),
            )
            .await
            .unwrap();
        let detail: ReadObjectDetail = serde_json::from_value(row.body).unwrap();
        assert_eq!(detail.reference, TraceAddress::of(&reference));
    }
}
async fn replay(order: SourceOrder, incremental: bool, early: bool) -> Projection {
    let fixture = Fixture::for_vendor("pi").await;
    let (mut store, _) = fixture.generation("pi-tools").await;
    let (body, bindings) = attachments(&fixture).await;
    if early {
        for capture in &bindings {
            fixture
                .evidence
                .admit_batch(
                    &capture.observation_ref.issuer,
                    std::slice::from_ref(capture),
                )
                .await
                .unwrap();
        }
        fixture
            .worker()
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        store = reopened(store).await;
    }
    for (index, record) in records(&CORPUS, order).iter().enumerate() {
        let capture = admit_record(&fixture, record).await;
        fixture
            .evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        if incremental {
            fixture
                .worker_with(WorkerLimits {
                    max_unit_observations: 1 + index % 4,
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            if index % 7 == 0 {
                store = reopened(store).await;
            }
        }
    }
    if !early {
        for capture in bindings {
            fixture
                .evidence
                .admit_batch(&capture.observation_ref.issuer, std::slice::from_ref(&capture))
                .await
                .unwrap();
        }
    }
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    expected(&fixture, &store, &body).await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("pi-tools-rebuilt").await;
    fixture.worker().catch_up(&fresh, 1, cut).await.unwrap();
    expected(&fixture, &fresh, &body).await;
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
async fn recorded_pi_tool_ids_and_all_sidecar_surfaces_survive_early_attachments_restart_and_rebuild(
) {
    for (raw, digest) in [
        (
            CORPUS.native,
            "e3001f8daca440bd5792ac45a3f94e7dafc35f6f99a7f95424cb5cc874e9591e",
        ),
        (
            CORPUS.live,
            "05a02a75564343b94509e119c78daccc23cfbe7af324174539e47a1b619ba17b",
        ),
        (
            BODY,
            "390829a80790cf03b6fc082ee651d43d17bdb707646f7e3cb2a340e235f29924",
        ),
    ] {
        assert_eq!(format!("{:x}", Sha256::digest(raw)), digest);
    }
    let cold = replay(SourceOrder::NativeFirst, false, false).await;
    assert_projection(&replay(SourceOrder::LiveFirst, true, true).await, &cold);
    assert_projection(&replay(SourceOrder::Alternating, true, true).await, &cold);
}
