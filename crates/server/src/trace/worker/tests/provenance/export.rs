use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use futures::StreamExt;

async fn export_response(
    f: &ProvenanceFixture,
    environment: &str,
    token: &str,
    suffix: &str,
) -> axum::response::Response {
    f.get_response(&format!(
        "/api/v1/trace/environments/{environment}/export?at_snapshot={token}{suffix}"
    ))
    .await
}
fn frames(bytes: &[u8]) -> Vec<Value> {
    assert_eq!(bytes.last(), Some(&b'\n'));
    let lines: Vec<_> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
    let values: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    let last = values.last().unwrap();
    assert_eq!(last["kind"], "complete");
    let prefix = lines[..lines.len() - 1].concat();
    assert_eq!(last["prefixBytes"], prefix.len());
    assert_eq!(last["frames"], lines.len() - 1);
    assert_eq!(last["sha256"], hex::encode(Sha256::digest(&prefix)));
    values
}
async fn setup() -> (ProvenanceFixture, AgentRef, String) {
    let mut f = ProvenanceFixture::new().await;
    let owner = agent("recipient", "export-reader");
    let identity = f
        .capture(
            &owner,
            "identity",
            br#" {"duplicate":1,"duplicate":2,"unknown":[true,null]} "#,
        )
        .await;
    f.publish(
        vec![claim(
            &owner,
            "agent",
            agent_object(&owner, "Export reader"),
            &[&identity],
        )],
        &[&identity],
    )
    .await;
    let token = f.bootstrap(&owner).await["view"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    (f, owner, token)
}

#[tokio::test]
async fn evidence_export_keeps_every_admission_exact_payload_and_fixed_environment_cut() {
    let (mut f, owner, _) = setup().await;
    let bytes = b" {\"type\":\"unknown_vendor_kind\",\"same\":1,\"same\":2} \r\n";
    let first = f.capture(&owner, "native", bytes).await;
    let mut repeated = first.clone();
    repeated.observation_ref.producer_id = "reread".into();
    f.state
        .trace
        .evidence()
        .admit_batch(
            &repeated.observation_ref.issuer,
            std::slice::from_ref(&repeated),
        )
        .await
        .unwrap();
    let other = agent("origin", "outside");
    let unrelated = f.capture(&other, "outside", b"PRIVATE OUTSIDE BYTES").await;
    f.publish(vec![], &[&first, &repeated, &unrelated]).await;
    let token = f.bootstrap(&owner).await["view"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let late = f.capture(&owner, "late", b"AFTER RETAINED CUT").await;
    let response = export_response(&f, "recipient", &token, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let values = frames(&body);
    assert_eq!(values[0]["environment"], "recipient");
    assert_eq!(
        values[0]["projectionManifest"],
        serde_json::to_value(f.generation.manifest()).unwrap()
    );
    let captures: Vec<_> = values
        .iter()
        .filter(|value| value["kind"] == "capture")
        .collect();
    assert_eq!(captures.len(), 3);
    let first_row = captures
        .iter()
        .find(|row| {
            row["record"]["capture"]["observationRef"]
                == serde_json::to_value(&first.observation_ref).unwrap()
        })
        .unwrap();
    let second_row = captures
        .iter()
        .find(|row| {
            row["record"]["capture"]["observationRef"]
                == serde_json::to_value(&repeated.observation_ref).unwrap()
        })
        .unwrap();
    assert_eq!(
        first_row["record"]["occurrenceId"],
        second_row["record"]["occurrenceId"]
    );
    assert_ne!(
        first_row["record"]["admissionSeq"],
        second_row["record"]["admissionSeq"]
    );
    let raw: Vec<Vec<u8>> = values
        .iter()
        .filter(|value| value["kind"] == "bytes")
        .map(|value| STANDARD.decode(value["data"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        raw.iter().filter(|value| value.as_slice() == bytes).count(),
        2
    );
    assert!(
        !captures
            .iter()
            .any(|row| row["record"]["capture"]["observationRef"]
                == serde_json::to_value(&late.observation_ref).unwrap())
    );
    assert!(
        !raw.iter()
            .any(|value| value == b"PRIVATE OUTSIDE BYTES" || value == b"AFTER RETAINED CUT")
    );
}

#[tokio::test]
async fn export_aborts_on_environment_revocation_or_deletion_but_survives_unrelated_deletion() {
    for change in ["grant", "delete", "other"] {
        let (mut f, owner, token) = setup().await;
        let outside = agent("origin", "unrelated");
        let _ = f.capture(&outside, "other", b"other data").await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *f.state.trace.evidence_export_pause.lock().await =
            Some((entered.clone(), release.clone()));
        let response = export_response(&f, "recipient", &token, "").await;
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        match change {
            "grant" => {
                f.state
                    .store
                    .revoke_grant(&f.subject, "recipient", "observe")
                    .await
                    .unwrap();
            }
            "delete" => {
                f.state
                    .trace
                    .evidence()
                    .delete_scope(&IngestionScope::Agent(owner))
                    .await
                    .unwrap();
            }
            _ => {
                f.state
                    .trace
                    .evidence()
                    .delete_scope(&IngestionScope::Agent(outside))
                    .await
                    .unwrap();
            }
        }
        release.notify_one();
        let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024).await;
        if change == "other" {
            frames(&body.unwrap());
        } else {
            assert!(body.is_err(), "{change}: revoked export cannot complete");
        }
    }
}

#[tokio::test]
async fn export_owns_its_view_and_disconnect_releases_stream_capacity() {
    let (f, _, token) = setup().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.state.trace.evidence_export_pause.lock().await = Some((entered.clone(), release.clone()));
    let response = export_response(&f, "recipient", &token, "").await;
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    f.state
        .trace
        .release_lease(&f.subject, &token)
        .await
        .unwrap();
    release.notify_one();
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    frames(&body);
    let token = f.bootstrap(&agent("recipient", "export-reader")).await["view"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut stream = export_response(&f, "recipient", &token, "")
        .await
        .into_body()
        .into_data_stream();
    assert!(stream.next().await.unwrap().is_ok());
    drop(stream);
    let maximum = f.state.trace.evidence_export_limits.max_jobs;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let permits: Vec<_> = (0..maximum)
                .filter_map(|_| f.state.trace.evidence_export_permit())
                .collect();
            if permits.len() == maximum {
                break;
            }
            drop(permits);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stalled_evidence_exports_obey_deadlines_and_never_return_a_short_success() {
    let (mut f, owner, _) = setup().await;
    let body = f
        .capture(&owner, "many-frames", &vec![b'a'; 512 * 1024])
        .await;
    f.publish(vec![], &[&body]).await;
    let token = f.bootstrap(&owner).await["view"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = export_response(&f, "recipient", &token, "&duration_ms=100").await;
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        f.state.trace.evidence_export_backpressure.notified(),
    )
    .await
    .unwrap();
    let duration = response.headers()["x-sylph-export-duration-ms"]
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(duration + 50)).await;
    let permits: Vec<_> = (0..f.state.trace.evidence_export_limits.max_jobs)
        .filter_map(|_| f.state.trace.evidence_export_permit())
        .collect();
    assert_eq!(
        permits.len(),
        f.state.trace.evidence_export_limits.max_jobs,
        "a stalled receiver cannot retain worker capacity past its deadline"
    );
    drop(permits);
    assert!(
        axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn sparse_attachment_export_never_reads_unreferenced_bytes_from_a_shared_physical_chunk() {
    let (mut f, owner, _) = setup().await;
    let other = agent("origin", "private-chunk");
    let shared = f
        .capture(&other, "shared-chunk", b"SECRET-PUBLIC-PRIVATE")
        .await;
    let manifest = PayloadManifest {
        chunks: vec![PayloadChunk {
            digest: shared.payload_ref.clone(),
            chunk_offset: 7,
            length: 6,
            payload_offset: 5,
        }],
        media_type: "application/octet-stream".into(),
        encoding: PayloadEncoding::Binary,
        decoded_length: Some(20),
        completeness: PayloadCompleteness::KnownRanges {
            ranges: vec![PayloadRange {
                start: 5,
                end_exclusive: 11,
            }],
        },
    };
    let payload = f
        .state
        .trace
        .evidence()
        .stage_manifest(&manifest)
        .await
        .unwrap();
    let mut source = f
        .capture(&owner, "attachment-source", b"attachment descriptor")
        .await;
    source.observation_ref.producer_id = "with-attachment".into();
    source.attachments.push(CaptureAttachment {
        locator: "/vendor/output".into(),
        source_position: None,
        observed_version: None,
        payload_ref: Some(payload.clone()),
        availability: AttachmentAvailability::Partial {
            reason: "Only the middle range survived".into(),
        },
        relation: "result".into(),
    });
    f.state
        .trace
        .evidence()
        .admit_batch(
            &source.observation_ref.issuer,
            std::slice::from_ref(&source),
        )
        .await
        .unwrap();
    f.publish(vec![], &[&shared, &source]).await;
    let token = f.bootstrap(&owner).await["view"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = axum::body::to_bytes(
        export_response(&f, "recipient", &token, "")
            .await
            .into_body(),
        8 * 1024 * 1024,
    )
    .await
    .unwrap();
    let values = frames(&body);
    let ranges = values
        .iter()
        .find(|value| value["kind"] == "payload" && value["payload"] == payload.as_str())
        .unwrap();
    assert_eq!(ranges["ranges"], json!([{"start":5,"endExclusive":11}]));
    let chunk = values
        .iter()
        .find(|value| value["kind"] == "bytes" && value["payload"] == payload.as_str())
        .unwrap();
    assert_eq!(chunk["offset"], 5);
    assert_eq!(
        STANDARD.decode(chunk["data"].as_str().unwrap()).unwrap(),
        b"PUBLIC"
    );
    assert!(
        !values
            .iter()
            .filter(|value| value["kind"] == "bytes")
            .any(|value| STANDARD
                .decode(value["data"].as_str().unwrap())
                .unwrap()
                .windows(6)
                .any(|bytes| bytes == b"SECRET"))
    );
}

#[tokio::test]
async fn expired_or_abandoned_export_jobs_release_their_typed_payload_pins() {
    use crate::trace::evidence::catalogue::{PayloadPin, PinOwnerKind};
    let (mut f, owner, _) = setup().await;
    let source = f.capture(&owner, "pinned-export", b"exported bytes").await;
    let cut = f.state.trace.evidence().evidence_cut().await.unwrap();
    for restart in [false, true] {
        let ticket = f
            .state
            .trace
            .evidence()
            .begin_export(
                owner.environment().clone(),
                cut,
                if restart { 300_000 } else { 20 },
            )
            .await
            .unwrap();
        f.state
            .trace
            .catalogue()
            .pin_payloads(
                PinOwnerKind::Export,
                &ticket.id,
                &[PayloadPin {
                    payload: source.payload_ref.clone(),
                    epoch: source.ingestion_authorization_epoch.clone(),
                }],
            )
            .await
            .unwrap();
        if !restart {
            while crate::models::now_ms() <= ticket.expires_at_ms {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
        assert_eq!(
            f.state
                .trace
                .catalogue()
                .retire_exports(restart, 32)
                .await
                .unwrap(),
            1
        );
        assert!(
            !f.state
                .trace
                .evidence()
                .export_is_current(&ticket)
                .await
                .unwrap()
        );
        assert!(
            f.state
                .trace
                .catalogue()
                .payload_pins(PinOwnerKind::Export, &ticket.id, None, 8)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn environment_export_pages_do_not_scan_unrelated_admissions_as_the_archive_grows() {
    let (mut f, owner, _) = setup().await;
    let other = agent("origin", "unrelated-load");
    let mut values = vec![];
    for count in [50, 500] {
        for index in 0..count {
            let _ = f
                .capture(&other, &format!("noise-{count}-{index}"), b"irrelevant")
                .await;
        }
        let target = f
            .capture(&owner, &format!("target-{count}"), b"local evidence")
            .await;
        let cut = f.state.trace.evidence().evidence_cut().await.unwrap();
        let receipt = f
            .state
            .trace
            .evidence()
            .receipt(&target.observation_ref)
            .await
            .unwrap()
            .unwrap();
        let ReceiptDisposition::Retained { admission_seq } = receipt.disposition else {
            panic!("retained capture")
        };
        let page = f
            .state
            .trace
            .evidence()
            .export_page(owner.environment(), admission_seq - 1, cut, 1)
            .await
            .unwrap();
        assert_eq!(page.captures.len(), 1);
        assert_eq!(page.candidates, 1);
        assert!(!page.more);
        assert!(page.vm_steps > 0);
        values.push(page.vm_steps);
    }
    assert!(
        values[1] <= values[0] + 50,
        "export read work grew with unrelated evidence: {values:?}"
    );
}

#[tokio::test]
async fn an_export_range_read_before_revocation_is_not_emitted_after_its_output_wait() {
    let (f, _, token) = setup().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.state.trace.evidence_export_payload_pause.lock().await =
        Some((entered.clone(), release.clone()));
    let response = export_response(&f, "recipient", &token, "").await;
    let consumer = tokio::spawn(async move {
        let mut stream = response.into_body().into_data_stream();
        let mut kinds = vec![];
        while let Some(frame) = stream.next().await {
            match frame {
                Ok(bytes) => {
                    let value: Value = serde_json::from_slice(&bytes).unwrap();
                    kinds.push(value["kind"].as_str().unwrap().to_owned());
                }
                Err(_) => return (kinds, true),
            }
        }
        (kinds, false)
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    f.state
        .store
        .revoke_grant(&f.subject, "recipient", "observe")
        .await
        .unwrap();
    release.notify_one();
    let (kinds, failed) = consumer.await.unwrap();
    assert!(failed);
    assert!(!kinds.contains(&"bytes".into()));
    assert!(!kinds.contains(&"complete".into()));
}

#[tokio::test]
async fn export_byte_budget_never_commits_a_partial_archive_or_widens_the_buffered_limit() {
    let (f, _, token) = setup().await;
    let response = export_response(&f, "recipient", &token, "&max_bytes=64").await;
    assert_eq!(response.headers()["x-sylph-export-limit"], "64");
    assert!(
        axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .is_err()
    );
    let over = f.state.trace.evidence_export_limits.max_buffered_bytes + 1;
    assert_eq!(
        export_response(
            &f,
            "recipient",
            &token,
            &format!("&buffered=true&max_bytes={over}")
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn evidence_export_preparation_uses_the_same_deadline_as_streaming() {
    let (f, _, token) = setup().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.state.trace.evidence_export_prepare_pause.lock().await =
        Some((entered.clone(), release.clone()));
    let app = f.app.clone();
    let cookie = f.cookie.clone();
    let request = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/trace/environments/recipient/export?at_snapshot={token}&duration_ms=20"
                ))
                .header(header::HOST, "localhost")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    release.notify_one();
    assert_eq!(request.await.unwrap().status(), StatusCode::REQUEST_TIMEOUT);
}

#[tokio::test]
async fn completed_export_output_and_capacity_do_not_wait_for_deferred_pin_cleanup() {
    let (f, _, token) = setup().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.state.trace.evidence_export_cleanup_pause.lock().await =
        Some((entered.clone(), release.clone()));
    let response = export_response(&f, "recipient", &token, "").await;
    let mut consumer =
        tokio::spawn(
            async move { axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024).await },
        );
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let complete = tokio::time::timeout(std::time::Duration::from_millis(100), &mut consumer).await;
    let maximum = f.state.trace.evidence_export_limits.max_jobs;
    let permits: Vec<_> = (0..maximum)
        .filter_map(|_| f.state.trace.evidence_export_permit())
        .collect();
    release.notify_one();
    let finished = complete.is_ok();
    let bytes = match complete {
        Ok(result) => result.unwrap().unwrap(),
        Err(_) => consumer.await.unwrap().unwrap(),
    };
    frames(&bytes);
    assert!(finished, "transport EOF cannot wait for pin cleanup");
    assert_eq!(permits.len(), maximum);
}

#[tokio::test]
async fn retiring_an_export_before_pin_publication_cannot_create_an_orphan_root() {
    use crate::trace::evidence::catalogue::{PayloadPin, PinOwnerKind};
    let (mut f, owner, _) = setup().await;
    let source = f.capture(&owner, "late-pin", b"retained bytes").await;
    let cut = f.state.trace.evidence().evidence_cut().await.unwrap();
    let ticket = f
        .state
        .trace
        .evidence()
        .begin_export(owner.environment().clone(), cut, 300_000)
        .await
        .unwrap();
    f.state
        .trace
        .catalogue()
        .retire_exports(true, 32)
        .await
        .unwrap();
    let result = f
        .state
        .trace
        .catalogue()
        .pin_export_payload(
            &ticket,
            PayloadPin {
                payload: source.payload_ref,
                epoch: source.ingestion_authorization_epoch,
            },
        )
        .await;
    assert!(
        result.is_err(),
        "pin publication must require the job that owns its recovery record"
    );
    assert!(
        f.state
            .trace
            .catalogue()
            .payload_pins(PinOwnerKind::Export, &ticket.id, None, 8)
            .await
            .unwrap()
            .is_empty()
    );
}

mod playback;
