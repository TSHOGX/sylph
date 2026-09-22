use super::*;

struct AccountingCase {
    vendor: &'static str,
    native: &'static [u8],
    live: &'static [u8],
}
const CASES: [AccountingCase; 3] = [
    AccountingCase {
        vendor: "codex",
        native: include_bytes!(
            "../../../../../../../fixtures/unified-trace/codex/accounting/native.jsonl"
        ),
        live: b"",
    },
    AccountingCase {
        vendor: "claude",
        native: include_bytes!(
            "../../../../../../../fixtures/unified-trace/claude/accounting/native.jsonl"
        ),
        live: include_bytes!(
            "../../../../../../../fixtures/unified-trace/claude/accounting/stdout.jsonl"
        ),
    },
    AccountingCase {
        vendor: "pi",
        native: include_bytes!(
            "../../../../../../../fixtures/unified-trace/pi/native-compaction.jsonl"
        ),
        live: include_bytes!(
            "../../../../../../../fixtures/unified-trace/pi/stdout-compaction.jsonl"
        ),
    },
];
async fn expected(case: &AccountingCase, store: &GenerationStore) {
    let reduced = reduction(&claims(store).await);
    let values: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Measurement(value)) => Some(value),
            _ => None,
        })
        .collect();
    assert!(values.iter().all(|value| value.components.is_empty()));
    match case.vendor {
        "codex" => {
            assert_eq!(
                values
                    .iter()
                    .filter(|value| value.metric == Metric::TotalTokens)
                    .count(),
                15
            );
            assert_eq!(
                values
                    .iter()
                    .filter(|value| matches!(value.subject, AnyObjectRef::Activation(_)))
                    .count(),
                18
            );
            assert!(reduced.executions.is_empty());
            let mut latest:Vec<_>=values.iter().filter(|value|value.metric==Metric::TotalTokens&&matches!(value.measurement_ref.address(),ObjectAddress::Occurrence{local_slot,..}if local_slot.get(1).is_some_and(|slot|slot=="latest"))).map(|value|value.value.as_i64().unwrap()).collect();
            latest.sort();
            assert_eq!(latest, [58679, 58710, 58804]);
        }
        "claude" => {
            let mut model: Vec<_> = values
                .iter()
                .filter(|value| value.metric == Metric::InputTokens && value.model.is_some())
                .map(|value| value.value.as_i64().unwrap())
                .collect();
            model.sort();
            assert_eq!(model, [10, 30]);
            let mut query: Vec<_> = values
                .iter()
                .filter(|value| {
                    value.metric == Metric::InputTokens
                        && matches!(value.subject, AnyObjectRef::Activation(_))
                })
                .map(|value| value.value.as_i64().unwrap())
                .collect();
            query.sort();
            assert_eq!(query, [10, 20]);
            let costs: Vec<_> = values
                .iter()
                .filter(|value| {
                    matches!(value.metric, Metric::Cost { .. }) && value.model.is_none()
                })
                .collect();
            assert_eq!(costs.len(), 2);
            assert!(
                costs
                    .iter()
                    .all(|value| matches!(value.subject, AnyObjectRef::Agent(_))
                        && matches!(value.mode, MeasurementMode::Cumulative { .. }))
            );
            for measurement in values.iter().filter(|value| value.model.is_some()) {
                let snapshot = store.checkpoint().await.unwrap().snapshot;
                let reference = AnyObjectRef::Measurement(measurement.measurement_ref.clone());
                let row = store
                    .read_row(
                        &snapshot,
                        &read_scope(reference.scope(), None, "objects"),
                        &reference.encode(),
                    )
                    .await
                    .unwrap();
                let detail: ReadObjectDetail = serde_json::from_value(row.body).unwrap();
                let ReadObjectContent::Measurement {
                    model: Some(model), ..
                } = detail.content
                else {
                    panic!("model scope must survive the reader wire contract");
                };
                assert_eq!(model.reported_name, "claude-sonnet-4-6");
                assert_eq!(model.provider.as_deref(), Some("firstParty"));
            }
        }
        "pi" => {
            let values: Vec<_> = values
                .iter()
                .filter(|value| matches!(value.subject, AnyObjectRef::ContextCheckpoint(_)))
                .collect();
            assert_eq!(values.len(), 12);
            assert!(
                values.iter().all(|value| value.value.as_f64() == Some(0.0)
                    && value.mode == MeasurementMode::Gauge)
            );
        }
        _ => unreachable!(),
    }
}
async fn replay(case: &AccountingCase, order: SourceOrder, incremental: bool) -> Projection {
    let fixture = Fixture::for_vendor(case.vendor).await;
    let (mut store, _) = fixture.generation("accounting").await;
    let mut captures = vec![];
    for (index, record) in source_records(case.native, case.live, order)
        .iter()
        .enumerate()
    {
        let capture = admit_record(&fixture, record).await;
        fixture
            .evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        captures.push(capture);
        if incremental {
            fixture
                .worker_with(WorkerLimits {
                    max_unit_observations: 1 + index % 4,
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            if index % 3 == 0 {
                store = reopened(store).await;
            }
        }
    }
    for mut capture in captures {
        capture.observation_ref.producer_id.push_str("-reread");
        fixture
            .evidence
            .admit_batch(&capture.observation_ref.issuer.clone(), &[capture])
            .await
            .unwrap();
    }
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    expected(case, &store).await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("accounting-rebuild").await;
    fixture.worker().catch_up(&fresh, 1, cut).await.unwrap();
    expected(case, &fresh).await;
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
async fn retained_vendor_accounting_keeps_scopes_models_and_counter_modes_through_replay() {
    for case in &CASES {
        let cold = replay(case, SourceOrder::NativeFirst, false).await;
        assert_projection(&replay(case, SourceOrder::LiveFirst, true).await, &cold);
        assert_projection(&replay(case, SourceOrder::Alternating, true).await, &cold);
    }
}
