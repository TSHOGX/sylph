use super::*;
use sylph_protocol::{NativeSourceState, SourceStateEvent};

async fn capture(
    service: &TraceService,
    base: &Capture,
    producer: &str,
    sequence: u64,
    transport: TransportKind,
    bytes: &[u8],
) {
    let mut capture = base.clone();
    capture.observation_ref.producer_id = producer.into();
    capture.observation_ref.producer_seq = sequence;
    capture.source_position = SourcePosition {
        source: SourceRef {
            issuer: base.observation_ref.issuer.clone(),
            instance: producer.into(),
        },
        generation: 0,
        locator: SourceLocator::Frame { ordinal: sequence },
    };
    capture.transport_kind = transport;
    capture.payload_ref = service.evidence.stage_payload(bytes).await.unwrap();
    capture.captured_at = TimeEvidence {
        domain: "server-receipt".into(),
        value: sequence as i64 * 1000,
        unit: TimeUnit::Milliseconds,
        precision: Some(1),
        basis: TimeBasis::ServerReceipt,
    };
    service
        .evidence
        .admit_batch(
            &capture.observation_ref.issuer,
            std::slice::from_ref(&capture),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn complete_native_prefix_never_retires_unique_live_bytes_clocks_or_fields() {
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    let header = NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    admit_native_from(&service, header, 0, 0).await;
    let base = service
        .evidence
        .admissions_after(0, 1)
        .await
        .unwrap()
        .captures
        .remove(0)
        .capture;
    let live = [
        b"{\"method\":\"thread/started\",\"params\":{\"thread\":{\"id\":\"response-user\"}}}\r\n".as_slice(),
        b"{\"method\":\"turn/started\",\"params\":{\"threadId\":\"response-user\",\"turn\":{\"id\":\"live-only\",\"startedAt\":41}}}\r\n".as_slice(),
        b" {\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"response-user\",\"turnId\":\"live-only\",\"itemId\":\"live-answer\",\"delta\":\"same delta\"},\"future\":1,\"future\":2} \r\n".as_slice(),
        b" {\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"response-user\",\"turnId\":\"live-only\",\"itemId\":\"live-answer\",\"delta\":\"same delta\"},\"future\":1,\"future\":2} \r\n".as_slice(),
    ];
    for (index, bytes) in live.iter().enumerate() {
        capture(
            &service,
            &base,
            "live-only",
            index as u64 + 1,
            TransportKind::LiveStdout,
            bytes,
        )
        .await;
    }
    service.project_pending().await.unwrap();
    let before = service.evidence.admissions_after(0, 64).await.unwrap();
    assert!(!before.has_more);
    let live_captures: Vec<_> = before
        .captures
        .into_iter()
        .map(|entry| entry.capture)
        .filter(|capture| capture.transport_kind == TransportKind::LiveStdout)
        .collect();
    assert_eq!(live_captures.len(), live.len());
    assert_eq!(live_captures[2].payload_ref, live_captures[3].payload_ref);
    assert_ne!(
        live_captures[2].source_position,
        live_captures[3].source_position
    );

    admit_native_from(&service, &NATIVE[header.len()..], 1, header.len() as u64).await;
    let complete = SourceStateEvent {
        source: base.source_position.source.clone(),
        generation: 0,
        complete_records: NATIVE.split_inclusive(|byte| *byte == b'\n').count() as u64,
        complete_bytes: NATIVE.len() as u64,
        pending_bytes: 0,
        following: false,
        state: NativeSourceState::Present,
    };
    capture(
        &service,
        &base,
        "native-prefix",
        1,
        TransportKind::SourceState,
        &serde_json::to_vec(&complete).unwrap(),
    )
    .await;
    service.project_pending().await.unwrap();
    let cut = service.evidence.evidence_cut().await.unwrap();
    let unrooted = service
        .evidence
        .stage_payload(b"collect this unreferenced sentinel")
        .await
        .unwrap();
    let collection = service.gc.run_once(crate::models::now_ms()).await.unwrap();
    assert!(
        collection.collected > 0,
        "the collector must actually run while the live evidence survives"
    );
    assert!(service
        .evidence
        .payload_bytes(&unrooted, 100)
        .await
        .is_err());
    service.rebuild().await.unwrap();
    let (_, reduction, _) = service
        .worker
        .projected_snapshot(&service.active().await.unwrap())
        .await
        .unwrap();
    assert!(
        reduction.coverage.iter().any(|(coverage, _)| {
            coverage.dimension == CoverageDimension::Messages
                && coverage.state == CoverageState::Complete
                && coverage.interval
                    == CoverageInterval::CapturedPrefix {
                        source: complete.source.clone(),
                        generation: complete.generation,
                        complete_records: complete.complete_records,
                        complete_bytes: complete.complete_bytes,
                    }
        }),
        "a complete native prefix must be interpreted, not merely retained unused"
    );
    close(service).await;

    let service = TraceService::open(directory.path()).await.unwrap();
    assert_eq!(service.evidence.evidence_cut().await.unwrap(), cut);
    let after = service.evidence.admissions_after(0, 64).await.unwrap();
    assert!(!after.has_more);
    for (original, raw) in live_captures.iter().zip(live) {
        let retained = after
            .captures
            .iter()
            .find(|entry| entry.capture.observation_ref == original.observation_ref)
            .expect("native completeness must not retire a live occurrence");
        assert_eq!(
            &retained.capture, original,
            "capture/source clocks and every envelope field must survive"
        );
        assert_eq!(
            service
                .evidence
                .capture_payload(&original.observation_ref, raw.len())
                .await
                .unwrap(),
            Some(raw.to_vec())
        );
        assert_eq!(
            service
                .evidence
                .receipt(&original.observation_ref)
                .await
                .unwrap()
                .unwrap()
                .observation_ref,
            original.observation_ref
        );
    }
    close(service).await;
}
