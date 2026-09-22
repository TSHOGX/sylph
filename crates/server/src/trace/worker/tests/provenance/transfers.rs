use super::*;
use std::io::{Cursor, Read};
use tokio::sync::Notify;

struct PayloadFixture {
    fixture: ProvenanceFixture,
    owner: AgentRef,
    access: PayloadAccess,
    token: String,
}
async fn payload_fixture(bytes: &[u8], gap: bool) -> PayloadFixture {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "transfer");
    let mut capture = fixture
        .capture(&owner, "body-proof", b"retained body acquisition")
        .await;
    let mut chunks = vec![];
    let mut offset = 0;
    for bytes in bytes.chunks(256 * 1024) {
        let expected = PayloadRef::bytes(blake3::hash(bytes).to_hex()).unwrap();
        let ticket = fixture
            .state
            .trace
            .catalogue()
            .begin_upload(
                crate::trace::evidence::catalogue::PinOwnerKind::Export,
                "transfer-fixture-staging",
                expected.clone(),
                capture.ingestion_authorization_epoch.clone(),
                crate::models::now_ms(),
            )
            .await
            .unwrap();
        let reference = fixture
            .state
            .trace
            .evidence()
            .stage_payload(bytes)
            .await
            .unwrap();
        assert_eq!(reference, expected);
        fixture
            .state
            .trace
            .catalogue()
            .finish_upload(&ticket)
            .await
            .unwrap();
        chunks.push(PayloadChunk {
            digest: reference,
            chunk_offset: 0,
            length: bytes.len() as u64,
            payload_offset: offset,
        });
        offset += bytes.len() as u64;
    }
    let source_manifest = PayloadManifest {
        chunks: chunks.clone(),
        media_type: "application/octet-stream".into(),
        encoding: PayloadEncoding::Binary,
        decoded_length: Some(offset),
        completeness: PayloadCompleteness::Complete,
    };
    if gap {
        for chunk in &mut chunks[1..] {
            chunk.payload_offset += 19;
        }
    }
    let ranges = if gap {
        vec![
            PayloadRange {
                start: 0,
                end_exclusive: chunks[0].length,
            },
            PayloadRange {
                start: chunks[1].payload_offset,
                end_exclusive: offset + 19,
            },
        ]
    } else {
        vec![PayloadRange {
            start: 0,
            end_exclusive: offset,
        }]
    };
    let manifest = PayloadManifest {
        chunks,
        media_type: "application/octet-stream".into(),
        encoding: PayloadEncoding::Binary,
        decoded_length: Some(offset + if gap { 19 } else { 0 }),
        completeness: if gap {
            PayloadCompleteness::KnownRanges { ranges }
        } else {
            PayloadCompleteness::Complete
        },
    };
    let ticket = fixture
        .state
        .trace
        .catalogue()
        .begin_upload(
            crate::trace::evidence::catalogue::PinOwnerKind::Export,
            "transfer-fixture-staging",
            PayloadRef::manifest(
                blake3::hash(&source_manifest.canonical_bytes().unwrap()).to_hex(),
            )
            .unwrap(),
            capture.ingestion_authorization_epoch.clone(),
            crate::models::now_ms(),
        )
        .await
        .unwrap();
    capture.payload_ref = fixture
        .state
        .trace
        .evidence()
        .stage_manifest(&source_manifest)
        .await
        .unwrap();
    fixture
        .state
        .trace
        .catalogue()
        .finish_upload(&ticket)
        .await
        .unwrap();
    capture.observation_ref.producer_id = "retained-transfer-body".into();
    capture.source_position.source.instance = "retained-transfer-body".into();
    fixture
        .state
        .trace
        .evidence()
        .admit_batch(
            &capture.observation_ref.issuer,
            std::slice::from_ref(&capture),
        )
        .await
        .unwrap();
    fixture
        .state
        .trace
        .catalogue()
        .release_owner_pins(
            crate::trace::evidence::catalogue::PinOwnerKind::Export,
            "transfer-fixture-staging",
            1024,
        )
        .await
        .unwrap();
    // A normalized partial representation can name gaps, while acquisition still
    // retains the complete source record and every byte it actually observed.
    let payload = if gap {
        let reference =
            PayloadRef::manifest(blake3::hash(&manifest.canonical_bytes().unwrap()).to_hex())
                .unwrap();
        let ticket = fixture
            .state
            .trace
            .catalogue()
            .begin_upload(
                crate::trace::evidence::catalogue::PinOwnerKind::Generation,
                &fixture.generation.manifest().generation,
                reference,
                capture.ingestion_authorization_epoch.clone(),
                crate::models::now_ms(),
            )
            .await
            .unwrap();
        let payload = fixture
            .state
            .trace
            .evidence()
            .stage_manifest(&manifest)
            .await
            .unwrap();
        fixture
            .state
            .trace
            .catalogue()
            .finish_upload(&ticket)
            .await
            .unwrap();
        payload
    } else {
        capture.payload_ref.clone()
    };
    let message = MessageRef::native(owner.clone(), "message", vec!["body".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(owner.clone(), "revision", vec!["body".into()]).unwrap();
    fixture
        .publish(
            vec![
                claim(
                    &owner,
                    "agent",
                    agent_object(&owner, "Transfer owner"),
                    &[&capture],
                ),
                claim(
                    &owner,
                    "message",
                    TraceObject::Message(Message {
                        message_ref: message.clone(),
                        author: Author::Unknown,
                        protocol_role: None,
                        purpose: MessagePurpose::Unclassified,
                        production: None,
                    }),
                    &[&capture],
                ),
                claim(
                    &owner,
                    "body",
                    TraceObject::MessageRevision(MessageRevision {
                        revision_ref: revision.clone(),
                        message,
                        representation: Representation::CallerResult,
                        form: RevisionForm::Complete,
                        parts: vec![ContentPart {
                            part_ref: MessagePartRef::native(
                                owner.clone(),
                                "part",
                                vec!["one".into()],
                            )
                            .unwrap(),
                            kind: ContentKind::Opaque,
                            payload,
                            media_type: Some("application/octet-stream".into()),
                            encoding: PayloadEncoding::Binary,
                        }],
                    }),
                    &[&capture],
                ),
            ],
            &[&capture],
        )
        .await;
    let boot = fixture.bootstrap(&owner).await;
    let token = boot["view"]["token"].as_str().unwrap().to_owned();
    let reference = TraceAddress::of(&AnyObjectRef::MessageRevision(revision));
    let (status, detail) = fixture
        .get(&format!(
            "/api/v1/trace/objects/{}?at_snapshot={token}",
            reference.as_str()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    let access: PayloadAccess =
        serde_json::from_value(detail["content"]["payloads"][0].clone()).unwrap();
    PayloadFixture {
        fixture,
        owner,
        access,
        token,
    }
}
impl PayloadFixture {
    fn url(&self, kind: &str) -> String {
        format!(
            "/api/v1/trace/payloads/{}/transfer?via_ref={}&at_snapshot={}&revision={}&representation=caller_result&kind={kind}",
            self.access.payload_ref,
            self.access.via_ref.as_str(),
            self.token,
            self.access.body_revision
        )
    }
    async fn pause(&self) -> (Arc<Notify>, Arc<Notify>) {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *self.fixture.state.trace.transfer_pause.lock().await =
            Some((entered.clone(), release.clone()));
        (entered, release)
    }
}
async fn wait(signal: &Notify) {
    tokio::time::timeout(std::time::Duration::from_secs(3), signal.notified())
        .await
        .unwrap();
}
#[tokio::test]
async fn explicit_transfer_streams_twenty_megabytes_exactly_and_preserves_gaps_in_zip() {
    let bytes: Vec<u8> = (0..20 * 1024 * 1024 + 11)
        .map(|index| (index % 251) as u8)
        .collect();
    let test = payload_fixture(&bytes, false).await;
    let response = test.fixture.get_response(&test.url("copy")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-sylph-observed-bytes"],
        bytes.len().to_string()
    );
    let actual = axum::body::to_bytes(response.into_body(), bytes.len() + 1)
        .await
        .unwrap();
    assert_eq!(blake3::hash(&actual), blake3::hash(&bytes));
    let gap = payload_fixture(&bytes[..700_000], true).await;
    let (status, body) = gap.fixture.get(&gap.url("copy")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "transfer_gap");
    let response = gap.fixture.get_response(&gap.url("download")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let result = axum::body::to_bytes(response.into_body(), 800_000)
        .await
        .unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(result)).unwrap();
    let manifest: Value = serde_json::from_reader(zip.by_name("manifest.json").unwrap()).unwrap();
    assert_eq!(manifest["observedBytes"], 700_000);
    assert_eq!(manifest["coverage"]["kind"], "known_ranges");
    assert_eq!(manifest["ranges"][1]["start"], 256 * 1024 + 19);
    for (name, expected) in [
        ("bytes/0-262144.bin", &bytes[..262144]),
        ("bytes/262163-700019.bin", &bytes[262144..700000]),
    ] {
        let mut actual = vec![];
        zip.by_name(name).unwrap().read_to_end(&mut actual).unwrap();
        assert_eq!(actual, expected);
    }
}
#[tokio::test]
async fn a_transfer_owns_its_snapshot_after_the_reader_releases_its_view() {
    let test = payload_fixture(b"held exact bytes", false).await;
    let (entered, release) = test.pause().await;
    let response = test.fixture.get_response(&test.url("copy")).await;
    assert_eq!(response.status(), StatusCode::OK);
    wait(&entered).await;
    test.fixture
        .state
        .trace
        .release_lease(&test.fixture.subject, &test.token)
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .as_ref(),
        b"held exact bytes"
    );
}
#[tokio::test]
async fn deleting_a_transfer_scope_aborts_the_stream_instead_of_returning_a_short_success() {
    let test = payload_fixture(b"private bytes", false).await;
    let (entered, release) = test.pause().await;
    let response = test.fixture.get_response(&test.url("copy")).await;
    assert_eq!(response.status(), StatusCode::OK);
    wait(&entered).await;
    test.fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(test.owner.clone()))
        .await
        .unwrap();
    release.notify_one();
    assert!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .is_err()
    );
}
async fn transfer_releases_capacity(disconnect: bool) {
    use sqlx::Connection;
    let test = payload_fixture(&vec![1; 700_000], false).await;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(test.fixture.state.config.data_dir.join("evidence.db")),
    )
    .await
    .unwrap();
    let (entered, _release) = test.pause().await;
    // A disconnected receiver must release promptly without borrowing the
    // deadline mechanism tested independently.
    let duration = if disconnect {
        test.fixture.state.trace.transfer_limits().lifetime_ms
    } else {
        500
    };
    let response = test
        .fixture
        .get_response(&format!("{}&duration_ms={duration}", test.url("copy")))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    wait(&entered).await;
    if disconnect {
        drop(response);
    } else {
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                axum::body::to_bytes(response.into_body(), 800_000)
            )
            .await
            .expect("transfer deadline was not enforced")
            .is_err()
        );
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let permits: Vec<_> = (0..test.fixture.state.trace.transfer_limits().max_jobs)
            .map(|_| test.fixture.state.trace.transfer_permit())
            .collect();
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM trace_view_leases WHERE state='active' AND lease_id<>?",
        )
        .bind(&test.token)
        .fetch_one(&mut connection)
        .await
        .unwrap();
        if permits.iter().all(Option::is_some) && active == 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "transfer capacity or its owned lease remained reserved"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_transfer_deadline_releases_worker_capacity_and_its_owned_lease() {
    transfer_releases_capacity(false).await;
}
#[tokio::test]
async fn a_disconnected_transfer_releases_worker_capacity_and_its_owned_lease() {
    transfer_releases_capacity(true).await;
}

#[tokio::test]
async fn revoking_observe_during_transfer_prevents_any_later_payload_chunk() {
    let test = payload_fixture(b"permission-sensitive bytes", false).await;
    let (entered, release) = test.pause().await;
    let response = test.fixture.get_response(&test.url("copy")).await;
    assert_eq!(response.status(), StatusCode::OK);
    wait(&entered).await;
    assert!(
        test.fixture
            .state
            .store
            .revoke_grant(&test.fixture.subject, "recipient", "observe")
            .await
            .unwrap()
    );
    release.notify_one();
    assert!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn transfer_requests_cannot_widen_body_revision_or_separately_reserved_capacity() {
    let test = payload_fixture(b"scoped body", false).await;
    let (status, error) = test.fixture.get(&test.url("unrecognized")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "invalid_cursor");
    let wrong = test
        .url("copy")
        .replace(&test.access.body_revision, "unobserved-revision");
    assert_ne!(
        test.fixture.get_response(&wrong).await.status(),
        StatusCode::OK
    );
    let permits: Vec<_> = (0..test.fixture.state.trace.transfer_limits().max_jobs)
        .map(|_| test.fixture.state.trace.transfer_permit().unwrap())
        .collect();
    let (status, error) = test.fixture.get(&test.url("copy")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error"]["code"], "transfer_capacity");
    drop(permits);
    let response = test.fixture.get_response(&test.url("copy")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .as_ref(),
        b"scoped body"
    );
}

#[tokio::test]
async fn revocation_while_a_full_output_queue_blocks_the_last_chunk_aborts_transfer() {
    let test = payload_fixture(&vec![1; 700_000], false).await;
    let response = test.fixture.get_response(&test.url("copy")).await;
    assert_eq!(response.status(), StatusCode::OK);
    // Do not consume the response yet. Two chunks fill the actual channel and
    // the final chunk waits for its slot after reading retained bytes.
    wait(&test.fixture.state.trace.transfer_backpressure).await;
    assert!(
        test.fixture
            .state
            .store
            .revoke_grant(&test.fixture.subject, "recipient", "observe")
            .await
            .unwrap()
    );
    assert!(
        axum::body::to_bytes(response.into_body(), 800_000)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn transfer_size_budgets_are_separate_from_range_reads_and_report_the_resolved_limit() {
    let configured = payload_fixture(b"limits", false).await;
    let limits = configured.fixture.state.trace.transfer_limits().clone();
    let over_copy = payload_fixture(&vec![0; limits.max_copy_bytes as usize + 1], false).await;
    let (status, error) = over_copy.fixture.get(&over_copy.url("copy")).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error["error"]["code"], "transfer_limit");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&limits.max_copy_bytes.to_string())
    );
    let over_buffer =
        payload_fixture(&vec![0; limits.max_buffered_bytes as usize + 1], false).await;
    let (status, error) = over_buffer
        .fixture
        .get(&format!("{}&buffered=true", over_buffer.url("download")))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error["error"]["code"], "transfer_limit");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&limits.max_buffered_bytes.to_string())
    );
}
#[tokio::test]
async fn an_observed_empty_representation_transfers_zero_bytes_without_inventing_missing_content() {
    let test = payload_fixture(b"", false).await;
    let response = test.fixture.get_response(&test.url("copy")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-sylph-observed-bytes"], "0");
    assert!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty()
    );
    let response = test.fixture.get_response(&test.url("download")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    assert_eq!(zip.by_name("bytes/0-0.bin").unwrap().size(), 0);
    let manifest: Value = serde_json::from_reader(zip.by_name("manifest.json").unwrap()).unwrap();
    assert_eq!(manifest["coverage"]["kind"], "complete");
    assert_eq!(manifest["totalBytes"], 0);
}

#[tokio::test]
async fn transfer_preparation_has_the_same_deadline_as_its_output_work() {
    use sqlx::Connection;
    let test = payload_fixture(b"preflight timeout", false).await;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(test.fixture.state.config.data_dir.join("evidence.db")),
    )
    .await
    .unwrap();
    let held = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        test.fixture
            .get(&format!("{}&duration_ms=100", test.url("copy"))),
    )
    .await
    .expect("preparation held transfer capacity past its deadline");
    assert_eq!(response.0, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(response.1["error"]["code"], "transfer_timeout");
    held.rollback().await.unwrap();
    let permits: Vec<_> = (0..test.fixture.state.trace.transfer_limits().max_jobs)
        .map(|_| test.fixture.state.trace.transfer_permit())
        .collect();
    assert!(permits.iter().all(Option::is_some));
}
