use super::*;

struct ResultsFixture {
    fixture: ProvenanceFixture,
    owner: AgentRef,
    contributor: AgentRef,
    operation: OperationRef,
    local: Capture,
    remote: Capture,
}
impl ResultsFixture {
    async fn new() -> Self {
        let mut fixture = ProvenanceFixture::new().await;
        let owner = agent("recipient", "result-reader");
        let contributor = agent("origin", "result-contributor");
        let local_bytes = b"Local result bytes";
        let remote_bytes = b"Separate result bytes";
        let local = fixture.capture(&owner, "local-results", local_bytes).await;
        let remote = fixture
            .capture(&contributor, "separate-results", remote_bytes)
            .await;
        for (source, size) in [(&local, local_bytes.len()), (&remote, remote_bytes.len())] {
            fixture.committed.previews.insert(
                source.payload_ref.clone(),
                Arc::new(sylph_trace::reading::PayloadPreview {
                    total_bytes: Some(size as u64),
                    ranges: vec![PayloadRange {
                        start: 0,
                        end_exclusive: size as u64,
                    }],
                    availability: Some(ReadAvailability::Available),
                    ..Default::default()
                }),
            );
        }
        let operation =
            OperationRef::native(owner.clone(), "operation", vec!["inspect".into()]).unwrap();
        let claims = vec![
            claim(&owner, "agent", agent_object(&owner, "Reader"), &[&local]),
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
                    input_revision: None,
                    argument_state: ArgumentState::Sealed,
                }),
                &[&local],
            ),
        ];
        fixture.publish(claims, &[&local, &remote]).await;
        Self {
            fixture,
            owner,
            contributor,
            operation,
            local,
            remote,
        }
    }
    fn reference(&self) -> String {
        AnyObjectRef::Operation(self.operation.clone()).encode()
    }
    fn result(
        &self,
        key: &str,
        representation: ResultRepresentation,
        remote: bool,
        has_payload: bool,
    ) -> Claim {
        let source = if remote { &self.remote } else { &self.local };
        claim(
            &self.owner,
            key,
            TraceObject::Result(ResultOccurrence {
                result_ref: ResultRef::native(self.owner.clone(), "result", vec![key.into()])
                    .unwrap(),
                operation: self.operation.clone(),
                recipient: None,
                disposition: ResultDisposition::FinalReturn,
                representation,
                payload: has_payload.then(|| PayloadUse::unclassified(source.payload_ref.clone())),
                delivery: None,
            }),
            &[source],
        )
    }
    async fn publish(&mut self, claims: Vec<Claim>) {
        self.fixture
            .publish(claims, &[&self.local, &self.remote])
            .await;
    }
    async fn token(&self) -> String {
        self.fixture.bootstrap(&self.owner).await["view"]["token"]
            .as_str()
            .unwrap()
            .into()
    }
    fn url(&self, token: &str) -> String {
        format!(
            "/api/v1/trace/objects/{}?at_snapshot={token}",
            self.reference()
        )
    }
}
fn choice(detail: &Value, representation: &str) -> Value {
    detail["content"]["resultRepresentations"]
        .as_array()
        .expect("result choices must not be inferred from the first occurrence page")
        .iter()
        .find(|entry| entry["representation"] == representation)
        .unwrap()
        .clone()
}

#[tokio::test]
async fn result_requests_refuse_submitted_input_and_probes_without_a_retained_view() {
    let f = ResultsFixture::new().await;
    let token = f.token().await;
    let path = format!("/api/v1/trace/objects/{}/relations", f.reference());
    let (status, _) = f
        .fixture
        .get(&format!(
            "{path}?at_snapshot={token}&kind=results&representation=submitted"
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "submitted input is not an operation result representation"
    );
    let (status, _) = f
        .fixture
        .get(&format!(
            "{path}?kind=result_representations&representation=runtime"
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a result catalogue continuation needs its retained view"
    );
}

#[tokio::test]
async fn an_observed_empty_result_is_an_available_representation() {
    let mut f = ResultsFixture::new().await;
    let empty = f.fixture.capture(&f.owner, "empty-result", b"").await;
    f.fixture.committed.previews.insert(
        empty.payload_ref.clone(),
        Arc::new(sylph_trace::reading::PayloadPreview {
            total_bytes: Some(0),
            availability: Some(ReadAvailability::Available),
            ..Default::default()
        }),
    );
    let value = claim(
        &f.owner,
        "empty-result",
        TraceObject::Result(ResultOccurrence {
            result_ref: ResultRef::native(f.owner.clone(), "result", vec!["empty".into()]).unwrap(),
            operation: f.operation.clone(),
            recipient: None,
            disposition: ResultDisposition::FinalReturn,
            representation: ResultRepresentation::CallerResult,
            payload: Some(PayloadUse::unclassified(empty.payload_ref.clone())),
            delivery: None,
        }),
        &[&empty],
    );
    f.fixture.publish(vec![value], &[&empty]).await;
    let token = f.token().await;
    let (status, detail) = f.fixture.get(&f.url(&token)).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        choice(&detail, "caller_result")["state"]["kind"],
        "available"
    );
}

#[tokio::test]
async fn a_grant_change_during_an_empty_catalogue_probe_requires_a_fresh_check() {
    let f = ResultsFixture::new().await;
    let token = f.token().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.fixture
        .state
        .trace
        .result_catalogue_read_pause
        .lock()
        .await = Some((entered.clone(), release.clone()));
    let app = f.fixture.app.clone();
    let cookie = f.fixture.cookie.clone();
    let url = f.url(&token);
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
        .store
        .revoke_grant(&f.fixture.subject, "origin", "observe")
        .await
        .unwrap();
    release.notify_one();
    let (status, body) = response(request.await.unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_cursor");
}

#[tokio::test]
async fn representation_catalogue_reaches_late_caller_results_and_pages_never_mix_representations()
{
    let mut f = ResultsFixture::new().await;
    let mut claims: Vec<_> = (0..20)
        .map(|index| {
            f.result(
                &format!("{index:04}"),
                ResultRepresentation::Runtime,
                false,
                true,
            )
        })
        .collect();
    claims.push(f.result("0020", ResultRepresentation::CallerResult, true, true));
    f.publish(claims).await;
    let token = f.token().await;
    let (status, detail) = f.fixture.get(&f.url(&token)).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        choice(&detail, "caller_result")["state"]["kind"],
        "available"
    );
    assert_eq!(
        choice(&detail, "conversation")["state"]["kind"],
        "unobserved"
    );
    assert!(
        detail["content"].get("results").is_none(),
        "mixed eager result previews must be removed"
    );
    assert!(detail["content"].get("resultsCursor").is_none());
    let base = format!(
        "/api/v1/trace/objects/{}/relations?at_snapshot={token}&kind=results",
        f.reference()
    );
    let (status, page) = f
        .fixture
        .get(&format!("{base}&representation=caller_result"))
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["representation"], "caller_result");
    assert_eq!(
        page["items"][0]["payloads"][0]["payloadRef"],
        f.remote.payload_ref.to_string()
    );
    let (status, page) = f
        .fixture
        .get(&format!("{base}&representation=runtime&limit=3"))
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 3);
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["representation"] == "runtime")
    );
    let cursor = page["afterCursor"].as_str().unwrap();
    let (status, _) = f
        .fixture
        .get(&format!(
            "{base}&representation=caller_result&after={cursor}"
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a runtime cursor cannot page caller results"
    );
    let (status, _) = f
        .fixture
        .get(&format!(
            "{}&representation=runtime&result_phase=occurrence&after={cursor}",
            base.replace("kind=results", "kind=result_representations")
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a result page cursor cannot certify catalogue absence"
    );
    let (status, _) = f.fixture.get(&base).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "mixed result reads are not a compatibility path"
    );
}

#[tokio::test]
async fn result_catalogue_contributors_do_not_own_the_operation_lease_and_missing_bytes_are_not_missing_results()
 {
    for delete in [false, true] {
        let mut f = ResultsFixture::new().await;
        f.publish(vec![
            f.result("remote", ResultRepresentation::CallerResult, true, true),
            f.result("missing", ResultRepresentation::Runtime, false, false),
        ])
        .await;
        let token = f.token().await;
        let (status, before) = f.fixture.get(&f.url(&token)).await;
        assert_eq!(status, StatusCode::OK, "{before}");
        assert_eq!(
            choice(&before, "caller_result")["state"]["kind"],
            "available"
        );
        assert_eq!(choice(&before, "runtime")["state"]["kind"], "unavailable");
        if delete {
            f.fixture
                .state
                .trace
                .evidence()
                .delete_scope(&IngestionScope::Agent(f.contributor.clone()))
                .await
                .unwrap();
        } else {
            f.fixture
                .state
                .store
                .revoke_grant(&f.fixture.subject, "origin", "observe")
                .await
                .unwrap();
        }
        let (status, after) = f.fixture.get(&f.url(&token)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "optional catalogue witnesses cannot expire the operation's own view: {after}"
        );
        assert_eq!(
            choice(&after, "caller_result")["state"]["kind"],
            "unobserved"
        );
        assert_eq!(choice(&after, "runtime")["state"]["kind"], "unavailable");
    }
}

#[tokio::test]
async fn a_result_witness_deleted_after_probing_cannot_authorize_a_catalogue_response() {
    let mut f = ResultsFixture::new().await;
    f.publish(vec![f.result(
        "remote",
        ResultRepresentation::CallerResult,
        true,
        true,
    )])
    .await;
    let token = f.token().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *f.fixture
        .state
        .trace
        .result_catalogue_read_pause
        .lock()
        .await = Some((entered.clone(), release.clone()));
    let app = f.fixture.app.clone();
    let cookie = f.fixture.cookie.clone();
    let url = f.url(&token);
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
        .delete_scope(&IngestionScope::Agent(f.contributor.clone()))
        .await
        .unwrap();
    release.notify_one();
    let (status, body) = response(request.await.unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_cursor");
    let (status, current) = f.fixture.get(&f.url(&token)).await;
    assert_eq!(status, StatusCode::OK, "{current}");
    assert_eq!(
        choice(&current, "caller_result")["state"]["kind"],
        "unobserved"
    );
}

#[tokio::test]
async fn result_presence_work_exhaustion_is_resumable_and_probe_cursors_cannot_page_content() {
    for has_payload in [true, false] {
        let mut f = ResultsFixture::new().await;
        let hidden = f.fixture.generation.limits().max_candidates + 17;
        let mut claims = vec![];
        for index in 0..hidden {
            claims.push(f.result(
                &format!("hidden-{index:04}"),
                ResultRepresentation::Runtime,
                true,
                has_payload,
            ));
            if claims.len() == 32 {
                f.publish(std::mem::take(&mut claims)).await;
            }
        }
        claims.push(f.result("visible", ResultRepresentation::Runtime, false, has_payload));
        f.publish(claims).await;
        f.fixture
            .state
            .store
            .revoke_grant(&f.fixture.subject, "origin", "observe")
            .await
            .unwrap();
        let token = f.token().await;
        let (status, initial) = f.fixture.get(&f.url(&token)).await;
        assert_eq!(status, StatusCode::OK, "{initial}");
        let pending = choice(&initial, "runtime");
        assert_eq!(
            pending["state"]["kind"], "pending",
            "a bounded skipped prefix does not prove absence"
        );
        let cursor = pending["state"]["cursor"].as_str().unwrap();
        let base = format!(
            "/api/v1/trace/objects/{}/relations?at_snapshot={token}&representation=runtime",
            f.reference()
        );
        let (status, _) = f
            .fixture
            .get(&format!("{base}&kind=results&after={cursor}"))
            .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a catalogue probe is not a content page"
        );
        let phase = pending["state"]["phase"].as_str().unwrap();
        let (status, completed) = f
            .fixture
            .get(&format!(
                "{base}&kind=result_representations&result_phase={phase}&after={cursor}"
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{completed}");
        assert_eq!(
            completed["state"]["kind"],
            if has_payload {
                "available"
            } else {
                "unavailable"
            }
        );
    }
}
