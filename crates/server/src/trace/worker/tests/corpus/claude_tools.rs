use super::*;
use sha2::{Digest, Sha256};
use sylph_protocol::SourceAttachmentEvent;

const READ: Corpus = Corpus {
    vendor: "claude",
    session: "a2bc4fad-e60f-4b37-9636-571b54f1973b",
    native: include_bytes!(
        "../../../../../../../fixtures/unified-trace/claude/tool-read/native.jsonl"
    ),
    live: include_bytes!(
        "../../../../../../../fixtures/unified-trace/claude/tool-read/stdout.jsonl"
    ),
    namespace: "claude.record",
    question_id: "e5379b93-f6bb-499e-abf8-8de57c93b744",
    answer_id: "9b4ea7d4-e0de-4fdc-ba32-fe7fb16dc7ac",
    question: b"Run the controlled local fixture tool and return the marker.",
    answer: b"SYLPH_CLAUDE_TOOL_OK",
};
const SIDECAR: Corpus = Corpus {
    vendor: "claude",
    session: "b824c239-d1a4-461f-b1ca-1f609355b400",
    native: include_bytes!(
        "../../../../../../../fixtures/unified-trace/claude/tool-sidecar/native.jsonl"
    ),
    live: include_bytes!(
        "../../../../../../../fixtures/unified-trace/claude/tool-sidecar/stdout.jsonl"
    ),
    namespace: "claude.record",
    question_id: "8f6eb122-808f-4b47-a2e3-8ae9b1ade268",
    answer_id: "37457ee8-d901-415e-a7e7-79c225ce1a44",
    question: b"Run the controlled local fixture tool and return the marker.",
    answer: b"SYLPH_CLAUDE_TOOL_OK",
};
const SIDECAR_BYTES: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/claude/tool-sidecar/sidecar.txt");
fn live_parent(corpus: &Corpus) -> ObservationRef {
    let ordinal = corpus
        .live
        .split_inclusive(|byte| *byte == b'\n')
        .position(|line| {
            serde_json::from_slice::<serde_json::Value>(line)
                .unwrap()
                .get("tool_use_result")
                .is_some()
        })
        .unwrap();
    ObservationRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
        producer_id: "corpus-live".into(),
        producer_epoch: "retained-capture".into(),
        producer_seq: ordinal as u64 + 1,
    }
}
async fn sidecar(fixture: &Fixture, parent: ObservationRef, sequence: u64) -> PayloadRef {
    let payload = fixture.stage(SIDECAR_BYTES).await;
    let issuer = parent.issuer.clone();
    let source = SourceRef {
        issuer: issuer.clone(),
        instance: "observed-sidecar".into(),
    };
    let position = SourcePosition {
        source,
        generation: 0,
        locator: SourceLocator::Snapshot {
            revision: "0".into(),
        },
    };
    let attachment = CaptureAttachment {
        locator: "/workspace/claude-capture/sidecar.txt".into(),
        source_position: Some(position.clone()),
        observed_version: Some("0".into()),
        payload_ref: Some(payload.clone()),
        availability: AttachmentAvailability::Captured,
        relation: "tool_output".into(),
    };
    let event = SourceAttachmentEvent {
        parent_observation: parent,
        parent_slot: "/message/content/0".into(),
        attachment: attachment.clone(),
    };
    let capture = Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: "sidecar-binding".into(),
            producer_epoch: "captured".into(),
            producer_seq: sequence,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer: issuer.clone(),
                instance: "sidecar-bindings".into(),
            },
            generation: 0,
            locator: SourceLocator::Frame {
                ordinal: sequence - 1,
            },
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
        vendor_build: Some("2.1.278".into()),
        schema_fingerprint: None,
        payload_ref: fixture.stage(&serde_json::to_vec(&event).unwrap()).await,
        attachments: vec![attachment],
        capture_diagnostics: vec![],
    };
    fixture
        .evidence
        .admit_batch(&issuer, &[capture])
        .await
        .unwrap();
    payload
}
async fn expected(
    corpus: &Corpus,
    fixture: &Fixture,
    store: &GenerationStore,
    has_sidecar: bool,
    sidecar_payload: Option<&PayloadRef>,
) {
    let retained = claims(store).await;
    let reduced = reduction(&retained);
    if has_sidecar {
        let bound: Vec<_> = reduced
            .objects
            .values()
            .filter_map(|object| match object.declaration.known() {
                Some(TraceObject::Result(result))
                    if result.payload.as_ref().map(|payload| &payload.reference)
                        == sidecar_payload =>
                {
                    Some(result)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            bound.len(),
            2,
            "one physical payload snapshot must keep both invocation results"
        );
        assert_ne!(bound[0].operation, bound[1].operation);
    }
    let operation = OperationRef::native(
        corpus.owner(),
        "claude.tool-call",
        vec!["toolu_controlled".into()],
    )
    .unwrap();
    let results: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Result(result)) if result.operation == operation => Some(result),
            _ => None,
        })
        .collect();
    let mut structured = vec![];
    let mut callers = vec![];
    let mut sidecars = 0;
    for result in results {
        let bytes = fixture
            .evidence
            .payload_bytes(&result.payload.as_ref().unwrap().reference, 256 * 1024)
            .await
            .unwrap();
        if result.payload.as_ref().map(|payload| &payload.reference) == sidecar_payload {
            assert_eq!(bytes, SIDECAR_BYTES);
            assert_eq!(result.representation, ResultRepresentation::Runtime);
            sidecars += 1;
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        match result.representation {
            ResultRepresentation::Runtime => structured.push(value),
            ResultRepresentation::CallerResult => callers.push(value),
            other => panic!("unexpected tool representation {other:?}"),
        }
    }
    assert_eq!(
        structured.len(),
        2,
        "both native and SDK observations retain their runtime representation"
    );
    assert_eq!(callers.len(), 2);
    assert_eq!(sidecars, usize::from(has_sidecar));
    assert_eq!(structured[0], structured[1]);
    if has_sidecar {
        assert_eq!(structured[0]["persistedOutputSize"], 172032);
        assert_eq!(
            structured[0]["persistedOutputPath"],
            "/workspace/claude-capture/sidecar.txt"
        );
        assert!(callers
            .iter()
            .all(|value| value["content"].as_str().unwrap().len() < SIDECAR_BYTES.len()));
        assert!(callers.iter().all(|value| value["content"]
            .as_str()
            .unwrap()
            .contains("/workspace/claude-capture/sidecar.txt")));
    } else {
        assert_eq!(
            structured[0]["file"]["content"],
            "SYLPH_CLAUDE_TOOL_BYTES\n"
        );
        assert!(callers
            .iter()
            .all(|value| value["content"] == "1\tSYLPH_CLAUDE_TOOL_BYTES\n2\t"));
    }
    for (id, body) in [
        (corpus.question_id, corpus.question),
        (corpus.answer_id, corpus.answer),
    ] {
        let bodies = revision_bytes(fixture, store, &corpus.message(id)).await;
        assert!(!bodies.is_empty());
        assert!(bodies.iter().all(|bytes| bytes == body));
    }
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
    let checkpoint = store.checkpoint().await.unwrap();
    let reference = AnyObjectRef::Operation(operation);
    let row = store
        .read_row(
            &checkpoint.snapshot,
            &read_scope(&corpus.owner(), None, "objects"),
            &reference.encode(),
        )
        .await
        .unwrap();
    let detail: ReadObjectDetail = serde_json::from_value(row.body).unwrap();
    assert_eq!(detail.reference, TraceAddress::of(&reference));
}
async fn replay(
    corpus: &Corpus,
    order: SourceOrder,
    incremental: bool,
    has_sidecar: bool,
) -> Projection {
    let fixture = Fixture::for_vendor("claude").await;
    let (mut store, _) = fixture.generation("claude-tools").await;
    let early = incremental && has_sidecar;
    let mut sidecar_payload = if early {
        Some(sidecar(&fixture, live_parent(corpus), 1).await)
    } else {
        None
    };
    if early {
        fixture
            .worker()
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        store = reopened(store).await;
    }
    let mut native_seen = false;
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
        native_seen |= record.source == "corpus-native";
        let live_tool = captured.observation_ref == live_parent(corpus);
        if incremental || live_tool {
            fixture
                .worker_with(WorkerLimits {
                    max_unit_observations: 1 + index % 3,
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            if live_tool && !native_seen {
                let reduced = reduction(&claims(&store).await);
                let runtime: Vec<_> = reduced
                    .objects
                    .values()
                    .filter_map(|object| match object.declaration.known() {
                        Some(TraceObject::Result(result))
                            if result.representation == ResultRepresentation::Runtime
                                && result.payload.as_ref().map(|payload| &payload.reference)
                                    != sidecar_payload.as_ref() =>
                        {
                            Some(result)
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    runtime.len(),
                    1,
                    "live structured metadata must be readable before native arrival"
                );
            }
            if index % 4 == 0 {
                store = reopened(store).await;
            }
        }
    }
    if has_sidecar && !early {
        sidecar_payload = Some(sidecar(&fixture, live_parent(corpus), 1).await);
    }
    if has_sidecar {
        // Synthetic second call reference: the same acquired snapshot is also
        // returned by another invocation. Its unobserved arguments stay unknown.
        let mut value: serde_json::Value = corpus
            .live
            .split_inclusive(|byte| *byte == b'\n')
            .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
            .find(|value| value.get("tool_use_result").is_some())
            .unwrap();
        value["uuid"] = "00000000-0000-4000-8000-000000000042".into();
        value["message"]["content"][0]["tool_use_id"] = "toolu_shared_snapshot".into();
        let issuer = live_parent(corpus).issuer;
        let parent = fixture
            .admit(
                ObservationRef {
                    issuer: issuer.clone(),
                    producer_id: "shared-snapshot-parent".into(),
                    producer_epoch: "captured".into(),
                    producer_seq: 1,
                },
                SourcePosition {
                    source: SourceRef {
                        issuer,
                        instance: "shared-snapshot-parent".into(),
                    },
                    generation: 0,
                    locator: SourceLocator::Frame { ordinal: 0 },
                },
                &serde_json::to_vec(&value).unwrap(),
                TransportKind::LiveStdout,
            )
            .await;
        sidecar(&fixture, parent.observation_ref, 2).await;
    }
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    expected(
        corpus,
        &fixture,
        &store,
        has_sidecar,
        sidecar_payload.as_ref(),
    )
    .await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("claude-tools-rebuilt").await;
    fixture.worker().catch_up(&fresh, 1, cut).await.unwrap();
    expected(
        corpus,
        &fixture,
        &fresh,
        has_sidecar,
        sidecar_payload.as_ref(),
    )
    .await;
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
async fn recorded_claude_tool_representations_and_late_sidecars_survive_source_orders_and_rebuild()
{
    for (corpus, has_sidecar, native, live) in [
        (
            &READ,
            false,
            "e1125df81806907736a73a78fe53a02eb613f5b23258257310a17b2d4fb1ca91",
            "0b40b41d3056cae1428ded6c5af737ab9408fa484cd7b013a1ba6ac3238a1939",
        ),
        (
            &SIDECAR,
            true,
            "c24e26e8183f07453b7fd3801ff67248ca1ea30fc7d34b00845b9bdb39091309",
            "2479c2b74c6c0a1b30e7d6997d66f505f38987d3208cf7ddceec361475071021",
        ),
    ] {
        assert_eq!(format!("{:x}", Sha256::digest(corpus.native)), native);
        assert_eq!(format!("{:x}", Sha256::digest(corpus.live)), live);
        let cold = replay(corpus, SourceOrder::NativeFirst, false, has_sidecar).await;
        assert_projection(
            &replay(corpus, SourceOrder::LiveFirst, true, has_sidecar).await,
            &cold,
        );
        assert_projection(
            &replay(corpus, SourceOrder::Alternating, true, has_sidecar).await,
            &cold,
        );
    }
    assert_eq!(
        format!("{:x}", Sha256::digest(SIDECAR_BYTES)),
        "b2d52d59a5b7cfe2a256ae39e5407f2b83ab142ed4815d4a1e529018e4360257"
    );
}
