use super::*;
use crate::trace::reading_store::read_scope;
use serde_json::json;

#[tokio::test]
async fn unknown_required_vendor_kind_retains_exact_bytes_and_an_evidenced_decode_gap() {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let unknown = b" {\"type\":\"response_item\",\"payload\":{\"type\":\"future_required_kind\",\"id\":\"future\",\"field\":1,\"field\":2}} \r\n";
    let capture = fixture.native(2, header.len() as u64, unknown).await;
    let (generation, _) = fixture.generation("unknown-kind").await;
    fixture.worker().catch_up(&generation, 1, 2).await.unwrap();
    let retained = claims(&generation).await;
    assert!(
        retained.iter().any(|claim| matches!(&claim.fact,
            Fact::Diagnostic { diagnostic } if diagnostic.code == DiagnosticCode::UnsupportedKind
                && diagnostic.message.contains("future_required_kind")
                && claim.evidence.contains(&capture.observation_ref)
        )),
        "an unknown required discriminator must remain a sourced diagnostic"
    );
    assert!(
        !retained.iter().any(|claim| matches!(&claim.fact,
            Fact::HistoryMappingComplete { .. } if claim.evidence.contains(&capture.observation_ref)
        )),
        "unrecognized content cannot certify an empty, completely mapped history interval"
    );
    assert!(!retained.iter().any(|claim| matches!(
        claim.fact,
        Fact::Declare {
            object: TraceObject::Message(_) | TraceObject::Activation(_)
        }
    )));
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&capture.observation_ref, unknown.len())
            .await
            .unwrap(),
        Some(unknown.to_vec())
    );
    let (rebuilt, _) = fixture.generation("unknown-kind-rebuilt").await;
    fixture.worker().catch_up(&rebuilt, 1, 2).await.unwrap();
    assert_eq!(claims(&rebuilt).await, retained);
}

#[tokio::test]
async fn receipt_clocks_never_fill_missing_native_execution_endpoints() {
    for (start, end) in [
        (None, None),
        (Some(41), None),
        (None, Some(44)),
        (Some(41), Some(44)),
    ] {
        let fixture = Fixture::new().await;
        let header = CODEX_NATIVE
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .unwrap();
        let header_capture = fixture.native(1, 0, header).await;
        let mut offset = header.len() as u64;
        for (seq, kind, key, stamp) in [
            (2, "task_started", "started_at", start),
            (3, "task_complete", "completed_at", end),
        ] {
            let mut payload = json!({"type":kind,"turn_id":"native-clock"});
            if let Some(stamp) = stamp {
                payload[key] = json!(stamp);
            }
            let mut raw =
                serde_json::to_vec(&json!({"type":"event_msg","payload":payload})).unwrap();
            raw.push(b'\n');
            let mut capture = header_capture.clone();
            capture.observation_ref.producer_seq = seq;
            capture.source_position.locator = SourceLocator::Jsonl {
                byte_start: offset,
                byte_end: offset + raw.len() as u64,
                record_ordinal: seq - 1,
            };
            capture.payload_ref = fixture.stage(&raw).await;
            capture.captured_at = TimeEvidence {
                domain: "server-receipt-only".into(),
                value: (seq * 1000) as i64,
                unit: TimeUnit::Milliseconds,
                precision: Some(1),
                basis: TimeBasis::ServerReceipt,
            };
            fixture
                .evidence
                .admit_batch(
                    &capture.observation_ref.issuer,
                    std::slice::from_ref(&capture),
                )
                .await
                .unwrap();
            offset += raw.len() as u64;
        }
        let (generation, _) = fixture.generation("native-clocks").await;
        let worker = fixture.worker();
        worker.catch_up(&generation, 1, 3).await.unwrap();
        let (_, reduction, _) = worker.projected_snapshot(&generation).await.unwrap();
        let activation = AnyObjectRef::Activation(
            ActivationRef::native(agent(), "codex.turn", vec!["native-clock".into()]).unwrap(),
        );
        let execution = reduction.execution(&activation).unwrap();
        let starts: Vec<_> = execution
            .boundaries
            .iter()
            .filter_map(|(boundary, _)| match boundary {
                ExecutionBoundary::Started { time } => Some(time.clone()),
                _ => None,
            })
            .collect();
        let ends: Vec<_> = execution
            .boundaries
            .iter()
            .filter_map(|(boundary, _)| match boundary {
                ExecutionBoundary::Terminated { time, .. } => Some(time.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts
                .iter()
                .map(|time| time.as_ref().map(|time| time.value))
                .collect::<Vec<_>>(),
            vec![start]
        );
        assert_eq!(
            ends.iter()
                .map(|time| time.as_ref().map(|time| time.value))
                .collect::<Vec<_>>(),
            vec![end]
        );
        for time in starts.iter().chain(&ends).flatten() {
            assert_eq!(time.basis, TimeBasis::SourceWall);
            assert_eq!(time.unit, TimeUnit::Seconds);
            assert_eq!(time.domain, "codex-wall:env7fk2");
        }
        let snapshot = generation.checkpoint().await.unwrap().snapshot;
        let page = generation
            .read_page(
                &snapshot,
                &read_scope(&agent(), None, "activations"),
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
        if start.is_some() && end.is_some() {
            assert_eq!(summary.durations.len(), 1);
            assert_eq!(summary.durations[0].value.as_i64(), Some(3));
            assert_eq!(summary.durations[0].unit, TimeUnit::Seconds);
        } else {
            assert!(
                summary.durations.is_empty(),
                "missing endpoints cannot acquire a plausible duration from receipts"
            );
        }
    }
}
