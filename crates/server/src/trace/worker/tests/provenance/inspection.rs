use super::*;

#[tokio::test]
async fn retained_evidence_pages_pin_claims_and_exact_source_bytes_without_reading_vendor_files() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "inspection");
    let bytes = b" {\"same\":1,\"same\":2} \r\n";
    let capture = fixture.capture(&owner, "exact-source", bytes).await;
    let long_name = "First name 中🙂".repeat(fixture.generation.limits().max_preview_bytes);
    let first = claim(
        &owner,
        "name-a",
        agent_object(&owner, &long_name),
        &[&capture],
    );
    fixture.publish(vec![first.clone()], &[&capture]).await;
    let boot = fixture.bootstrap(&owner).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let reference = TraceAddress::of(&AnyObjectRef::Agent(owner.clone()))
        .as_str()
        .to_owned();
    let url = format!("/api/v1/trace/objects/{reference}/evidence?at_snapshot={token}&limit=1");
    let reply = fixture.get_response(&url).await;
    assert_eq!(reply.status(), StatusCode::OK);
    let (_, page) = response(reply).await;
    assert_eq!(page["items"][0]["claimRef"], first.claim_ref.encode());
    assert_eq!(page["items"][0]["previewTruncated"], true);
    assert!(
        page["items"][0]["preview"].as_str().unwrap().len()
            <= fixture.generation.limits().max_preview_bytes
    );

    let claim_url = format!(
        "/api/v1/trace/objects/{reference}/claims/{}?at_snapshot={token}",
        first.claim_ref.encode()
    );
    let (status, detail) = fixture.get(&claim_url).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["claim"]["ruleId"], "normalized-provenance-test");
    assert_eq!(
        detail["claim"]["fact"]["object"]["object"]["name"],
        long_name
    );

    let source_url = format!(
        "/api/v1/trace/objects/{reference}/claims/{}/sources/{}?at_snapshot={token}&start=0&end={}",
        first.claim_ref.encode(),
        capture.observation_ref.encode(),
        bytes.len()
    );
    let (status, source) = fixture.get(&source_url).await;
    assert_eq!(status, StatusCode::OK, "{source}");
    use base64::Engine;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(source["bytesBase64"].as_str().unwrap())
            .unwrap(),
        bytes
    );
    assert_eq!(
        source["capture"]["sourcePosition"],
        serde_json::to_value(&capture.source_position).unwrap()
    );
    let mut second = claim(
        &owner,
        "name-b",
        agent_object(&owner, "Corrected name"),
        &[&capture],
    );
    second.supersedes.push(first.claim_ref.clone());
    fixture.publish(vec![second.clone()], &[&capture]).await;
    let (_, frozen) = fixture.get(&url).await;
    assert_eq!(frozen["items"], page["items"]);
    assert_eq!(frozen["hasAfter"], false);
    let current = fixture.bootstrap(&owner).await;
    let current_token = current["view"]["token"].as_str().unwrap();
    let (status, corrections) = fixture
        .get(&format!(
            "/api/v1/trace/objects/{reference}/evidence?at_snapshot={current_token}&corrects={}",
            first.claim_ref.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{corrections}");
    assert_eq!(corrections["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        corrections["items"][0]["claimRef"],
        second.claim_ref.encode()
    );
    let (_, detail) = fixture
        .get(&format!(
            "/api/v1/trace/objects/{reference}/claims/{}?at_snapshot={current_token}",
            second.claim_ref.encode()
        ))
        .await;
    assert_eq!(detail["claim"]["supersedes"][0], first.claim_ref.encode());
    let (_, old_corrections) = fixture
        .get(&format!("{url}&corrects={}", first.claim_ref.encode()))
        .await;
    assert_eq!(old_corrections["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn evidence_cursors_bind_the_view_and_subject_and_denied_claims_do_not_leak_metadata() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "evidence-visibility");
    let other = agent("origin", "private-evidence");
    let local = fixture.capture(&owner, "local", b"local exact bytes").await;
    let private = fixture
        .capture(&other, "private", b"private exact bytes")
        .await;
    let visible = claim(
        &owner,
        "z-visible",
        agent_object(&owner, "Visible"),
        &[&local],
    );
    let hidden = claim(
        &owner,
        "a-private",
        agent_object(&owner, "Private"),
        &[&private],
    );
    fixture
        .publish(vec![visible.clone(), hidden.clone()], &[&local, &private])
        .await;
    // A conflict has no ordinary object row, but its authorized evidence remains inspectable.
    let view = fixture
        .state
        .trace
        .lease_guard(&fixture.subject, None)
        .await
        .unwrap();
    let token = view.token.clone();
    view.retain();
    let reference = TraceAddress::of(&AnyObjectRef::Agent(owner.clone()))
        .as_str()
        .to_owned();
    let url = format!("/api/v1/trace/objects/{reference}/evidence?at_snapshot={token}&limit=1");
    let (status, first) = fixture.get(&url).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert!(
        first["items"][0]["preview"]
            .as_str()
            .unwrap()
            .contains("Private")
    );

    let cursor = first["afterCursor"].as_str().unwrap();
    assert!(!cursor.contains("private"));
    let (other_cookie, other_subject) = register(&fixture.app, "other-evidence-reader").await;
    fixture
        .state
        .store
        .issue_grant(
            &other_subject,
            "recipient",
            "observe",
            &fixture.subject,
            None,
        )
        .await
        .unwrap();
    let reply = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&url)
                .header(header::HOST, "localhost")
                .header(header::COOKIE, &other_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        reply.status(),
        StatusCode::FORBIDDEN,
        "a grant cannot borrow another subject's retained view"
    );

    let fresh = fixture
        .state
        .trace
        .lease_guard(&fixture.subject, None)
        .await
        .unwrap();
    let fresh_token = fresh.token.clone();
    fresh.retain();
    let (status, error) = fixture
        .get(&format!(
            "/api/v1/trace/objects/{reference}/evidence?at_snapshot={fresh_token}&after={cursor}"
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    fixture
        .state
        .store
        .revoke_grant(&fixture.subject, "origin", "observe")
        .await
        .unwrap();
    let (status, error) = fixture.get(&format!("{url}&after={cursor}")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "grant partition invalidates the cursor: {error}"
    );
    let (status, filtered) = fixture.get(&url).await;
    assert_eq!(status, StatusCode::OK, "{filtered}");
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["items"][0]["claimRef"], visible.claim_ref.encode());
    assert!(
        filtered["items"][0]["preview"]
            .as_str()
            .unwrap()
            .contains("Visible")
    );
    assert!(!filtered.to_string().contains(&hidden.claim_ref.encode()));
    let hidden_url = format!(
        "/api/v1/trace/objects/{reference}/claims/{}?at_snapshot={token}",
        hidden.claim_ref.encode()
    );
    assert_eq!(
        fixture.get_response(&hidden_url).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn source_ranges_require_exact_claim_membership_and_stop_after_revocation_or_deletion() {
    use base64::Engine;
    for deleted in [false, true] {
        let mut fixture = ProvenanceFixture::new().await;
        let owner = agent("recipient", "source-access");
        let bytes = b"\x00\xff retained binary \r\n".repeat(5_000);
        let source = fixture.capture(&owner, "source", &bytes).await;
        let unrelated = fixture.capture(&owner, "unrelated", b"other secret").await;
        let fact = claim(&owner, "one", agent_object(&owner, "Source"), &[&source]);
        fixture
            .publish(vec![fact.clone()], &[&source, &unrelated])
            .await;
        let boot = fixture.bootstrap(&owner).await;
        let token = boot["view"]["token"].as_str().unwrap();
        let reference = TraceAddress::of(&AnyObjectRef::Agent(owner.clone()))
            .as_str()
            .to_owned();
        let base = format!(
            "/api/v1/trace/objects/{reference}/claims/{}/sources/",
            fact.claim_ref.encode()
        );
        let url = format!(
            "{base}{}?at_snapshot={token}",
            source.observation_ref.encode()
        );
        let (status, range) = fixture.get(&format!("{url}&start=1&end=8")).await;
        assert_eq!(status, StatusCode::OK, "{range}");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(range["bytesBase64"].as_str().unwrap())
                .unwrap(),
            bytes[1..8]
        );
        assert_eq!(
            fixture
                .get_response(&format!(
                    "{base}{}?at_snapshot={token}",
                    unrelated.observation_ref.encode()
                ))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let maximum = range["maxRangeBytes"].as_u64().unwrap();
        assert!((bytes.len() as u64) > maximum);
        assert_eq!(
            fixture
                .get_response(&format!("{url}&start=0&end={}", maximum + 1))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            fixture
                .get_response(&format!("{url}&end={}", bytes.len() + 1))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        if deleted {
            fixture
                .state
                .trace
                .evidence()
                .delete_scope(&IngestionScope::Agent(owner))
                .await
                .unwrap();
        } else {
            fixture
                .state
                .store
                .revoke_grant(&fixture.subject, "recipient", "observe")
                .await
                .unwrap();
        }
        assert_ne!(fixture.get_response(&url).await.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn evidence_summary_pages_preserve_claim_order_and_empty_sources_remain_distinct() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "many-claims");
    let source = fixture.capture(&owner, "empty-source", b"").await;
    let mut facts = vec![];
    for index in 0..18 {
        facts.push(claim(
            &owner,
            &format!("claim-{index:02}"),
            agent_object(&owner, "Agreeing"),
            &[&source],
        ));
    }
    fixture.publish(facts.clone(), &[&source]).await;
    let boot = fixture.bootstrap(&owner).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let reference = TraceAddress::of(&AnyObjectRef::Agent(owner.clone()))
        .as_str()
        .to_owned();
    let base = format!("/api/v1/trace/objects/{reference}/evidence?at_snapshot={token}&limit=3");
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    loop {
        let url = cursor
            .as_ref()
            .map_or_else(|| base.clone(), |cursor| format!("{base}&after={cursor}"));
        let (status, page) = fixture.get(&url).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(page["items"].as_array().unwrap().len() <= 3);
        for item in page["items"].as_array().unwrap() {
            assert!(seen.insert(item["claimRef"].as_str().unwrap().to_owned()));
        }
        if !page["hasAfter"].as_bool().unwrap() {
            break;
        }
        cursor = Some(page["afterCursor"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        seen,
        facts.iter().map(|claim| claim.claim_ref.encode()).collect()
    );
    let (status, empty) = fixture
        .get(&format!(
            "/api/v1/trace/objects/{reference}/claims/{}/sources/{}?at_snapshot={token}",
            facts[0].claim_ref.encode(),
            source.observation_ref.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["totalBytes"], 0);
    assert_eq!(empty["bytesBase64"], "");
}

#[tokio::test]
async fn denied_evidence_candidates_exhaust_work_with_a_private_resumable_position() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "denied-candidates");
    let other = agent("origin", "denied-origin");
    let local = fixture.capture(&owner, "local-work", b"local").await;
    let private = fixture.capture(&other, "hidden-work", b"hidden").await;
    let maximum = fixture.generation.limits().max_candidates;
    let visible = claim(
        &owner,
        "zzzz-visible",
        agent_object(&owner, "Same"),
        &[&local],
    );
    let mut claims: Vec<_> = (0..maximum / 2 + 4)
        .map(|index| {
            claim(
                &owner,
                &format!("aaaa-{index:07}"),
                agent_object(&owner, "Same"),
                &[&private],
            )
        })
        .collect();
    assert!(
        claims
            .iter()
            .all(|claim| claim.claim_ref.encode() < visible.claim_ref.encode())
    );
    claims.push(visible.clone());
    fixture.publish(claims, &[&local, &private]).await;
    fixture
        .state
        .store
        .revoke_grant(&fixture.subject, "origin", "observe")
        .await
        .unwrap();
    let lease = fixture
        .state
        .trace
        .lease_guard(&fixture.subject, None)
        .await
        .unwrap();
    let token = lease.token.clone();
    lease.retain();
    let reference = TraceAddress::of(&AnyObjectRef::Agent(owner))
        .as_str()
        .to_owned();
    let url = format!("/api/v1/trace/objects/{reference}/evidence?at_snapshot={token}&limit=1");
    let (status, first) = fixture.get(&url).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["state"], "work_budget_exhausted");
    assert!(first["items"].as_array().unwrap().is_empty());
    assert_eq!(first["hasAfter"], true);
    let cursor = first["afterCursor"].as_str().unwrap();
    let (status, next) = fixture.get(&format!("{url}&after={cursor}")).await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["items"][0]["claimRef"], visible.claim_ref.encode());
    assert_eq!(next["hasAfter"], false);
}

#[tokio::test]
async fn claim_metadata_remains_inspectable_when_source_bytes_are_corrupt_and_range_reads_fail() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "corrupt-source");
    let capture = fixture
        .capture(&owner, "source", b"retained source body")
        .await;
    let fact = claim(
        &owner,
        "evidence",
        agent_object(&owner, "Metadata"),
        &[&capture],
    );
    fixture.publish(vec![fact.clone()], &[&capture]).await;
    let boot = fixture.bootstrap(&owner).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let digest = capture.payload_ref.digest();
    let path = fixture
        .state
        .config
        .data_dir
        .join("blobs")
        .join(&digest[..2])
        .join(&digest[2..4])
        .join(digest);
    tokio::fs::write(&path, b"corrupt retained bytes")
        .await
        .unwrap();
    let reference = TraceAddress::of(&AnyObjectRef::Agent(owner))
        .as_str()
        .to_owned();
    let reply = fixture
        .get_response(&format!(
            "/api/v1/trace/objects/{reference}/evidence?at_snapshot={token}"
        ))
        .await;
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(
        reply.headers()[header::CACHE_CONTROL],
        "private, no-store, no-transform"
    );
    let base = format!(
        "/api/v1/trace/objects/{reference}/claims/{}",
        fact.claim_ref.encode()
    );
    assert_eq!(
        fixture
            .get_response(&format!("{base}?at_snapshot={token}"))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .get_response(&format!(
                "{base}/sources/{}?at_snapshot={token}",
                capture.observation_ref.encode()
            ))
            .await
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn a_grant_revoked_after_retained_source_read_prevents_the_response() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "in-flight-evidence");
    let source_owner = agent("origin", "support");
    let local = fixture.capture(&owner, "local", b"local").await;
    let source = fixture
        .capture(&source_owner, "source", b"private returned bytes")
        .await;
    let fact = claim(
        &owner,
        "both",
        agent_object(&owner, "Evidence"),
        &[&local, &source],
    );
    fixture
        .publish(vec![fact.clone()], &[&local, &source])
        .await;
    let boot = fixture.bootstrap(&owner).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let reference = TraceAddress::of(&AnyObjectRef::Agent(owner))
        .as_str()
        .to_owned();
    let url = format!(
        "/api/v1/trace/objects/{reference}/claims/{}/sources/{}?at_snapshot={token}",
        fact.claim_ref.encode(),
        source.observation_ref.encode()
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *fixture.state.trace.evidence_read_pause.lock().await =
        Some((entered.clone(), release.clone()));
    let app = fixture.app.clone();
    let cookie = fixture.cookie.clone();
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
    fixture
        .state
        .store
        .revoke_grant(&fixture.subject, "origin", "observe")
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(request.await.unwrap().status(), StatusCode::NOT_FOUND);
}
