use super::*;
use sqlx::Connection;

async fn playback_response(
    f: &ProvenanceFixture,
    token: &str,
    suffix: &str,
) -> axum::response::Response {
    f.get_response(&format!(
        "/api/v1/trace/environments/recipient/playback?at_snapshot={token}{suffix}"
    ))
    .await
}

#[tokio::test]
async fn playback_keeps_receipt_order_intermediate_bytes_and_explicit_pacing_discontinuities() {
    let (mut f, owner, _) = setup().await;
    let mut captures = Vec::new();
    for (index, bytes) in [b"A".as_slice(), b"A", b"B", b"BC"].into_iter().enumerate() {
        captures.push(f.capture(&owner, &format!("revision-{index}"), bytes).await);
    }
    f.publish(vec![], &captures.iter().collect::<Vec<_>>())
        .await;
    let token = f.bootstrap(&owner).await["view"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    // Deliberately authored receipt clocks differ from capture/source clocks.
    // Mutation of only the fixture database isolates pacing from machine timing.
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(f.state.config.data_dir.join("evidence.db")),
    )
    .await
    .unwrap();
    for (index, time) in [1000i64, 1010, 1010, 900, 90000].into_iter().enumerate() {
        sqlx::query("UPDATE observations SET admitted_at_ms=? WHERE admission_seq=?")
            .bind(time)
            .bind((index + 1) as i64)
            .execute(&mut connection)
            .await
            .unwrap();
    }
    f.capture(&owner, "after-cut", b"MUST NOT PLAY").await;
    let response = playback_response(&f, &token, "&speed=2&max_gap_ms=20").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let values = frames(&body);
    assert_eq!(values[0]["pacing"]["clock"], "server_admission");
    assert_eq!(values[0]["pacing"]["speed"], 2);
    assert_eq!(values[0]["pacing"]["maxGapMs"], 20);
    let timing: Vec<_> = values.iter().filter(|v| v["kind"] == "timing").collect();
    assert_eq!(timing.len(), 5);
    assert_eq!(
        timing
            .iter()
            .map(|v| v["delayMs"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 5, 0, 0, 20]
    );
    assert_eq!(
        timing
            .iter()
            .map(|v| v["adjustment"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "first",
            "recorded",
            "recorded",
            "backward_clock",
            "gap_clamped"
        ]
    );
    assert_eq!(
        timing
            .iter()
            .map(|v| v["recordedDeltaMs"].as_i64())
            .collect::<Vec<_>>(),
        [None, Some(10), Some(0), Some(-110), Some(89100)]
    );
    for index in 1..values.len() {
        if values[index]["kind"] == "capture" {
            assert_eq!(values[index - 1]["kind"], "timing");
            assert_eq!(
                values[index - 1]["admissionSeq"],
                values[index]["record"]["admissionSeq"]
            );
        }
    }
    let bytes: Vec<_> = values
        .iter()
        .filter(|v| v["kind"] == "bytes")
        .map(|v| STANDARD.decode(v["data"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        &bytes[1..],
        &[b"A".to_vec(), b"A".to_vec(), b"B".to_vec(), b"BC".to_vec()]
    );
}

#[tokio::test]
async fn playback_has_independent_capacity_and_owns_its_retained_view() {
    let (f, _, token) = setup().await;
    let exports: Vec<_> = (0..f.state.trace.evidence_export_limits.max_jobs)
        .map(|_| f.state.trace.evidence_export_permit().unwrap())
        .collect();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.state.trace.evidence_export_pause.lock().await = Some((entered.clone(), release.clone()));
    let response = playback_response(&f, &token, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-sylph-export-limit"]
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap(),
        f.state.trace.evidence_playback_limits.max_output_bytes
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        playback_response(&f, &token, "").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    f.state
        .trace
        .catalogue()
        .expire_lease(&f.subject, &token)
        .await
        .unwrap();
    release.notify_one();
    frames(
        &axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap(),
    );
    assert!(f.state.trace.evidence_playback_permit().is_some());
    drop(exports);
}

#[tokio::test]
async fn playback_refuses_widened_policy_and_revocation_prevents_remaining_frames() {
    let (f, _, token) = setup().await;
    for suffix in [
        "&speed=0",
        "&speed=3",
        "&max_gap_ms=0",
        "&max_gap_ms=2001",
        "&buffered=true",
        "&duration_ms=300001",
        "&max_bytes=999999999",
    ] {
        assert_eq!(
            playback_response(&f, &token, suffix).await.status(),
            StatusCode::BAD_REQUEST,
            "{suffix}"
        );
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.state.trace.evidence_export_pause.lock().await = Some((entered.clone(), release.clone()));
    let response = playback_response(&f, &token, "").await;
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    f.state
        .store
        .revoke_grant(&f.subject, "recipient", "observe")
        .await
        .unwrap();
    release.notify_one();
    assert!(
        axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .is_err()
    );
    assert!(f.state.trace.evidence_playback_permit().is_some());
}

#[test]
fn receipt_clock_overflow_and_small_scaled_gaps_remain_explicit() {
    use crate::trace::evidence_export::Pacing;
    let policy = Pacing {
        clock: "server_admission",
        speed: 10,
        max_gap_ms: 2000,
    };
    let overflow = policy.timing(2, Some(i64::MIN), i64::MAX);
    assert_eq!(overflow.recorded_delta_ms, None);
    assert_eq!(overflow.adjustment, "unrepresentable_delta");
    assert_eq!(overflow.delay_ms, 0);
    assert_eq!(policy.timing(3, Some(1000), 1001).delay_ms, 1);
}

#[tokio::test]
async fn playback_clock_metadata_preserves_integer_lexemes_for_browser_consumers() {
    let (mut f, owner, _) = setup().await;
    let mut capture = f.capture(&owner, "clock-record", b"original bytes").await;
    capture.observation_ref.producer_id = "nanosecond-clock-capture".into();
    capture.source_clock = Some(TimeEvidence {
        domain: "native-wall".into(),
        value: 1_700_000_000_000_000_001,
        unit: TimeUnit::Nanoseconds,
        precision: Some(1),
        basis: TimeBasis::SourceWall,
    });
    f.state
        .trace
        .evidence()
        .admit_batch(
            &capture.observation_ref.issuer,
            std::slice::from_ref(&capture),
        )
        .await
        .unwrap();
    f.publish(vec![], &[&capture]).await;
    let token = f.bootstrap(&owner).await["view"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = playback_response(&f, &token, "").await;
    let values = frames(
        &axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap(),
    );
    let record = values
        .iter()
        .find(|v| {
            v["kind"] == "capture"
                && v["record"]["capture"]["observationRef"]
                    == serde_json::to_value(&capture.observation_ref).unwrap()
        })
        .unwrap();
    assert_eq!(
        record["record"]["capture"]["sourceClock"]["value"].as_i64(),
        Some(1_700_000_000_000_000_001)
    );
    let lexical = record["metadataText"]["sourceClock"]
        .as_str()
        .expect("Playback metadata must remain exact text in a browser");
    assert!(lexical.contains("\"value\":1700000000000000001"));
    assert!(lexical.contains("\"unit\":\"nanoseconds\""));
}
