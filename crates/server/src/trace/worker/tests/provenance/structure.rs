use super::*;

struct Structured {
    fixture: ProvenanceFixture,
    owner: AgentRef,
    payload: PayloadRef,
    token: String,
    reference: String,
}
impl Structured {
    async fn new(bytes: &[u8]) -> Self {
        let mut fixture = ProvenanceFixture::new().await;
        let owner = agent("recipient", "structured");
        let source = if bytes.len() <= fixture.state.trace.evidence().limits().max_chunk_bytes {
            fixture.capture(&owner, "json", bytes).await
        } else {
            let mut source = fixture
                .capture(&owner, "manifest-header", b"manifest")
                .await;
            let width = fixture.state.trace.evidence().limits().max_chunk_bytes;
            let mut chunks = vec![];
            for (index, bytes) in bytes.chunks(width).enumerate() {
                let expected = PayloadRef::bytes(blake3::hash(bytes).to_hex()).unwrap();
                let ticket = fixture
                    .state
                    .trace
                    .catalogue()
                    .begin_upload(
                        crate::trace::evidence::catalogue::PinOwnerKind::Export,
                        "structure-fixture",
                        expected.clone(),
                        source.ingestion_authorization_epoch.clone(),
                        crate::models::now_ms(),
                    )
                    .await
                    .unwrap();
                let digest = fixture
                    .state
                    .trace
                    .evidence()
                    .stage_payload(bytes)
                    .await
                    .unwrap();
                assert_eq!(digest, expected);
                fixture
                    .state
                    .trace
                    .catalogue()
                    .finish_upload(&ticket)
                    .await
                    .unwrap();
                chunks.push(PayloadChunk {
                    digest,
                    chunk_offset: 0,
                    payload_offset: (index * width) as u64,
                    length: bytes.len() as u64,
                });
            }
            let manifest = PayloadManifest {
                chunks,
                media_type: "application/json".into(),
                encoding: PayloadEncoding::Utf8,
                decoded_length: Some(bytes.len() as u64),
                completeness: PayloadCompleteness::Complete,
            };
            let expected =
                PayloadRef::manifest(blake3::hash(&manifest.canonical_bytes().unwrap()).to_hex())
                    .unwrap();
            let ticket = fixture
                .state
                .trace
                .catalogue()
                .begin_upload(
                    crate::trace::evidence::catalogue::PinOwnerKind::Export,
                    "structure-fixture",
                    expected,
                    source.ingestion_authorization_epoch.clone(),
                    crate::models::now_ms(),
                )
                .await
                .unwrap();
            source.payload_ref = fixture
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
            source.observation_ref.producer_id = "json-manifest".into();
            source.source_position.source.instance = "json-manifest".into();
            fixture
                .state
                .trace
                .evidence()
                .admit_batch(
                    &source.observation_ref.issuer,
                    std::slice::from_ref(&source),
                )
                .await
                .unwrap();
            fixture
                .state
                .trace
                .catalogue()
                .release_owner_pins(
                    crate::trace::evidence::catalogue::PinOwnerKind::Export,
                    "structure-fixture",
                    1024,
                )
                .await
                .unwrap();
            source
        };
        let operation =
            OperationRef::native(owner.clone(), "operation", vec!["json".into()]).unwrap();
        fixture
            .publish(
                vec![
                    claim(
                        &owner,
                        "agent",
                        agent_object(&owner, "JSON reader"),
                        &[&source],
                    ),
                    claim(
                        &owner,
                        "operation",
                        TraceObject::Operation(Operation {
                            operation_ref: operation.clone(),
                            owner: OwnerRef::Agent {
                                agent: owner.clone(),
                            },
                            activation: None,
                            requester: None,
                            category: OperationCategory::Wait,
                            input_revision: Some(PayloadUse {
                                reference: source.payload_ref.clone(),
                                format: PayloadFormat {
                                    media_type: Some("application/json".into()),
                                    encoding: Some(PayloadEncoding::Utf8),
                                },
                            }),
                            argument_state: ArgumentState::Sealed,
                        }),
                        &[&source],
                    ),
                ],
                &[&source],
            )
            .await;
        let token = fixture.bootstrap(&owner).await["view"]["token"]
            .as_str()
            .unwrap()
            .to_owned();
        Self {
            fixture,
            owner,
            payload: source.payload_ref,
            token,
            reference: AnyObjectRef::Operation(operation).encode(),
        }
    }
    fn create_url(&self) -> String {
        format!(
            "/api/v1/trace/payloads/{}/structure?at_snapshot={}&via_ref={}&revision={}&representation=runtime",
            self.payload, self.token, self.reference, self.payload
        )
    }
    async fn request(&self, method: &str, url: &str) -> axum::response::Response {
        self.fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(url)
                    .header(header::HOST, "localhost")
                    .header(header::COOKIE, &self.fixture.cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    async fn start(&self) -> Value {
        let (status, value) = response(self.request("POST", &self.create_url()).await).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{value}");
        value
    }
    async fn ready(&self, id: &str) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let (status, value) = self
                    .fixture
                    .get(&format!("/api/v1/trace/structures/{id}"))
                    .await;
                assert_eq!(status, StatusCode::OK, "{value}");
                if value["state"] != "building" {
                    return value;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn structured_pages_preserve_duplicate_members_exact_numbers_and_an_independent_retained_view()
 {
    let mut members = vec![
        r#""same":[1e4000,-0.00]"#.to_owned(),
        r#""same":{"nested":"hello"}"#.to_owned(),
    ];
    for i in 0..90 {
        members.push(format!("\"key-{i}\":{i}"));
    }
    let bytes = format!("{{{}}}", members.join(","));
    let f = Structured::new(bytes.as_bytes()).await;
    let started = f.start().await;
    let id = started["id"].as_str().unwrap();
    assert_eq!(
        f.request("DELETE", &format!("/api/v1/trace/views/{}", f.token))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let page = f.ready(id).await;
    assert_eq!(page["state"], "ready", "{page}");
    let limit = page["limits"]["pageItems"].as_u64().unwrap() as usize;
    assert_eq!(page["items"].as_array().unwrap().len(), limit);
    assert_eq!(page["node"]["children"], 92);
    assert_eq!(page["items"][0]["name"]["text"], "\"same\"");
    assert_eq!(page["items"][1]["name"]["text"], "\"same\"");
    assert_ne!(page["items"][0]["id"], page["items"][1]["id"]);
    let child = page["items"][0]["id"].as_u64().unwrap();
    let (status, nested) = f
        .fixture
        .get(&format!("/api/v1/trace/structures/{id}?node={child}"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(nested["items"][0]["preview"]["text"], "1e4000");
    assert_eq!(nested["items"][1]["preview"]["text"], "-0.00");
    let (status, next) = f
        .fixture
        .get(&format!(
            "/api/v1/trace/structures/{id}?node=1&after={}",
            page["after"]
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(next["items"][0]["ordinal"], limit);
    assert_eq!(
        f.request("DELETE", &format!("/api/v1/trace/structures/{id}"))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.fixture
            .get(&format!("/api/v1/trace/structures/{id}"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let owned = started["view"]["token"].as_str().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if f.fixture
                .state
                .trace
                .catalogue()
                .resolve_lease(&f.fixture.subject, owned, crate::models::now_ms())
                .await
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn malformed_structured_content_reports_failure_without_publishing_its_valid_prefix() {
    let f = Structured::new(br#"{"ok":[1,2]} false"#).await;
    let started = f.start().await;
    let page = f.ready(started["id"].as_str().unwrap()).await;
    assert_eq!(page["state"], "failed");
    assert!(page.get("items").is_none());
    assert!(page["error"].as_str().is_some_and(|text| !text.is_empty()));
}

#[tokio::test]
async fn cancelling_a_blocked_index_releases_capacity_without_reading_the_remaining_source() {
    let f = Structured::new(br#"[1,2,3]"#).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.fixture.state.trace.structure_scan_pause.lock().await =
        Some((entered.clone(), release.clone()));
    let started = f.start().await;
    let id = started["id"].as_str().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let job = f
        .fixture
        .state
        .trace
        .structures
        .get(id, &f.fixture.subject)
        .await
        .unwrap();
    assert_eq!(
        f.request("DELETE", &format!("/api/v1/trace/structures/{id}"))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if matches!(
                *job.state.read().await,
                crate::trace::structure::State::Failed(_)
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(job.bytes.load(std::sync::atomic::Ordering::Relaxed), 0);
    drop(job);
    let maximum = f.fixture.state.trace.structures.limits.max_jobs;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let permits: Vec<_> = (0..maximum)
                .filter_map(|_| f.fixture.state.trace.structures.reserve())
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
async fn deletion_during_a_structural_page_read_prevents_returning_the_prepared_page() {
    let f = Structured::new(br#"{"private":"retained"}"#).await;
    let started = f.start().await;
    let id = started["id"].as_str().unwrap();
    assert_eq!(f.ready(id).await["state"], "ready");
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.fixture.state.trace.structure_page_pause.lock().await =
        Some((entered.clone(), release.clone()));
    let app = f.fixture.app.clone();
    let cookie = f.fixture.cookie.clone();
    let url = format!("/api/v1/trace/structures/{id}");
    let request = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .uri(url)
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
    f.fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(f.owner.clone()))
        .await
        .unwrap();
    release.notify_one();
    let (status, body) = response(request.await.unwrap()).await;
    assert_ne!(status, StatusCode::OK);
    assert!(body.get("items").is_none());
}

#[tokio::test]
async fn twenty_megabyte_scalar_keeps_its_index_and_first_page_bounded() {
    let mut bytes = b"{\"large\":\"".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 20 * 1024 * 1024));
    bytes.extend_from_slice(b"\",\"tail\":123456789012345678901234567890}");
    let f = Structured::new(&bytes).await;
    let started = f.start().await;
    let ready = f.ready(started["id"].as_str().unwrap()).await;
    assert_eq!(ready["state"], "ready", "{ready}");
    assert_eq!(ready["nodes"], 3);
    assert_eq!(ready["bytesRead"], bytes.len());
    let preview_limit = ready["limits"]["previewBytes"].as_u64().unwrap() as usize;
    let first = &ready["items"][0];
    assert!(first["preview"]["text"].as_str().unwrap().len() <= preview_limit);
    assert_eq!(first["preview"]["truncated"], true);
    assert_eq!(
        ready["items"][1]["preview"]["text"],
        "123456789012345678901234567890"
    );
    assert!(serde_json::to_vec(&ready).unwrap().len() < preview_limit * 16);
}

#[tokio::test]
async fn structural_jobs_enforce_subject_ownership_capacity_and_explicit_lifetime() {
    let f = Structured::new(b"[1]").await;
    let count = f.fixture.state.trace.structures.limits.max_jobs;
    let mut jobs = vec![];
    for _ in 0..count {
        jobs.push(f.start().await);
    }
    assert_eq!(
        f.request("POST", &f.create_url()).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let (cookie, _) = register(&f.fixture.app, "other-structure-reader").await;
    let path = format!(
        "/api/v1/trace/structures/{}",
        jobs[0]["id"].as_str().unwrap()
    );
    for method in ["GET", "DELETE"] {
        let response = f
            .fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .header(header::HOST, "localhost")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if method == "GET" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::NO_CONTENT
            }
        );
    }
    assert_eq!(f.fixture.get(&path).await.0, StatusCode::OK);
    assert_eq!(
        f.request("DELETE", &path).await.status(),
        StatusCode::NO_CONTENT
    );
    for job in jobs.iter().skip(1) {
        f.request(
            "DELETE",
            &format!("/api/v1/trace/structures/{}", job["id"].as_str().unwrap()),
        )
        .await;
    }
    // Wait for cancelled builders to drop their permits rather than assuming a
    // DELETE response synchronously stops a blocking parser thread.
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if f.fixture.state.trace.structures.reserve().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (status, started) = response(
        f.request("POST", &format!("{}&duration_ms=500", f.create_url()))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let deadline = started["expiresAtMs"].as_i64().unwrap();
    while crate::models::now_ms() <= deadline {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        f.fixture
            .get(&format!(
                "/api/v1/trace/structures/{}",
                started["id"].as_str().unwrap()
            ))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
