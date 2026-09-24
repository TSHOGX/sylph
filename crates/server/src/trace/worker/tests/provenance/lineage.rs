use super::*;

#[tokio::test]
async fn a_fork_origin_link_requires_both_its_lineage_proof_and_its_target_metadata() {
    for change in ["lineage_deleted", "target_deleted", "lineage_grant_revoked"] {
        let mut fixture = ProvenanceFixture::new().await;
        let recipient = agent("recipient", "fork-reader");
        let origin = agent("recipient", "original-reader");
        let witness = agent("origin", "lineage-witness");
        let local = fixture
            .capture(&recipient, "fork", b"recipient identity")
            .await;
        let target = fixture
            .capture(&origin, "original", b"original identity")
            .await;
        let proof = fixture
            .capture(&witness, "lineage", b"independent origin assertion")
            .await;
        let mut lineage = claim(
            &recipient,
            "origin",
            agent_object(&recipient, "unused"),
            &[&proof],
        );
        lineage.fact = Fact::Relation {
            relation: Relation::ContextLineage {
                recipient: recipient.clone(),
                checkpoint: None,
                original_owner: OwnerRef::Agent {
                    agent: origin.clone(),
                },
            },
        };
        fixture
            .publish(
                vec![
                    claim(
                        &recipient,
                        "identity",
                        agent_object(&recipient, "Fork reader"),
                        &[&local],
                    ),
                    claim(
                        &origin,
                        "identity",
                        agent_object(&origin, "Original reader"),
                        &[&target],
                    ),
                    lineage,
                ],
                &[&local, &target, &proof],
            )
            .await;
        let before = fixture.bootstrap(&recipient).await;
        assert_eq!(before["header"]["origin"]["address"], origin.encode());
        assert_eq!(before["header"]["origin"]["name"], "Original reader");
        assert!(before["header"]["parent"].is_null());
        assert_eq!(before["header"]["descendantCount"]["value"], 0);
        let token = before["view"]["token"].as_str().unwrap();
        match change {
            "lineage_deleted" => {
                fixture
                    .state
                    .trace
                    .evidence()
                    .delete_scope(&IngestionScope::Agent(witness))
                    .await
                    .unwrap();
            }
            "target_deleted" => {
                fixture
                    .state
                    .trace
                    .evidence()
                    .delete_scope(&IngestionScope::Agent(origin))
                    .await
                    .unwrap();
            }
            "lineage_grant_revoked" => {
                fixture
                    .state
                    .store
                    .revoke_grant(&fixture.subject, "origin", "observe")
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let (status, retained) = fixture
            .get(&format!(
                "/api/v1/trace/agents/{}?at_snapshot={token}",
                recipient.encode()
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{change}: {retained}");
        assert!(
            retained["header"]["origin"].is_null(),
            "{change}: {retained}"
        );
        assert_eq!(retained["header"]["agent"]["name"], "Fork reader");
        assert!(!retained.to_string().contains("Original reader"));
    }
}

#[tokio::test]
async fn origin_catalogue_preserves_multiple_unresolved_and_unavailable_assertions() {
    for deleted in [false, true] {
        let mut f = ProvenanceFixture::new().await;
        let recipient = agent("recipient", "origin-catalogue");
        let original = agent("origin", "readable-original");
        let absent = agent("origin", "absent-original");
        let capture = f
            .capture(&recipient, "origins", b"retained fork claims")
            .await;
        let target = f
            .capture(&original, "origin-target", b"original identity")
            .await;
        let mut claims = vec![
            claim(
                &recipient,
                "agent",
                agent_object(&recipient, "Fork"),
                &[&capture],
            ),
            claim(
                &original,
                "target",
                agent_object(&original, "Original"),
                &[&target],
            ),
        ];
        for (index, owner) in [
            OwnerRef::Agent {
                agent: original.clone(),
            },
            OwnerRef::Agent { agent: absent },
            OwnerRef::Unresolved {
                scope: recipient.clone(),
                namespace: "native-origin".into(),
                native_key: vec!["missing".into()],
                reason: "No binding".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let mut proof = claim(
                &recipient,
                &format!("origin-{index}"),
                agent_object(&recipient, "unused"),
                &[&capture],
            );
            proof.fact = Fact::Relation {
                relation: Relation::ContextLineage {
                    recipient: recipient.clone(),
                    checkpoint: None,
                    original_owner: owner,
                },
            };
            claims.push(proof);
        }
        f.publish(claims, &[&capture, &target]).await;
        let boot = f.bootstrap(&recipient).await;
        let token = boot["view"]["token"].as_str().unwrap();
        assert!(boot["header"]["origin"].is_null());
        let path = format!(
            "/api/v1/trace/agents/{}/origins?at_snapshot={token}&limit=2",
            recipient.encode()
        );
        let (status, first) = f.get(&path).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let rows = first["items"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["origin"]["name"], "Original");
        assert!(rows[1]["origin"].is_null());
        assert_eq!(rows[1]["availability"]["kind"], "unavailable");
        let cursor = first["afterCursor"].as_str().unwrap();
        let (status, next) = f.get(&format!("{path}&after={cursor}")).await;
        assert_eq!(status, StatusCode::OK, "{next}");
        assert_eq!(next["items"][0]["availability"]["kind"], "unavailable");
        assert!(next["items"][0]["origin"].is_null());
        assert!(next["items"][0]["claim"].as_str().is_some());
        if deleted {
            f.state
                .trace
                .evidence()
                .delete_scope(&IngestionScope::Agent(original.clone()))
                .await
                .unwrap();
            f.state
                .trace
                .evidence()
                .readopt_scope(
                    &IngestionScope::Agent(original.clone()),
                    std::slice::from_ref(&original),
                )
                .await
                .unwrap();
        } else {
            f.state
                .store
                .revoke_grant(&f.subject, "origin", "observe")
                .await
                .unwrap();
        }

        let (status, hidden) = f.get(&path).await;
        assert_eq!(status, StatusCode::OK, "{hidden}");
        assert!(hidden["items"][0]["origin"].is_null());
        assert!(!hidden.to_string().contains("Original"));
        assert!(!hidden.to_string().contains(&original.encode()));
        let (status, child) = f
            .get(&format!(
                "/api/v1/trace/agents/{}?at_snapshot={token}",
                recipient.encode()
            ))
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "origin inspection cannot lend target retention to the child: {child}"
        );
    }
}

#[tokio::test]
async fn origin_assertions_use_their_own_proof_epochs_instead_of_combining_endpoint_support() {
    let mut f = ProvenanceFixture::new().await;
    let recipient = agent("recipient", "independent-origins");
    let original = agent("recipient", "original");
    let witness = agent("origin", "separate-proof");
    let local = f.capture(&recipient, "local", b"local proof").await;
    let foreign = f.capture(&witness, "foreign", b"separate proof").await;
    let mut claims = vec![
        claim(
            &recipient,
            "agent",
            agent_object(&recipient, "Fork"),
            &[&local],
        ),
        claim(
            &original,
            "agent",
            agent_object(&original, "Original"),
            &[&local],
        ),
    ];
    for (key, source) in [("local-origin", &local), ("foreign-origin", &foreign)] {
        let mut proof = claim(
            &recipient,
            key,
            agent_object(&recipient, "unused"),
            &[source],
        );
        proof.fact = Fact::Relation {
            relation: Relation::ContextLineage {
                recipient: recipient.clone(),
                checkpoint: None,
                original_owner: OwnerRef::Agent {
                    agent: original.clone(),
                },
            },
        };
        claims.push(proof);
    }
    f.publish(claims, &[&local, &foreign]).await;
    f.committed = f
        .state
        .trace
        .worker()
        .load_state(&f.generation, f.committed.committed_snapshot())
        .await
        .unwrap()
        .0;
    f.publish(vec![], &[]).await;
    let boot = f.bootstrap(&recipient).await;
    let token = boot["view"]["token"].as_str().unwrap();
    // Do not inspect the foreign assertion before deletion; a newly opened
    // catalogue cannot borrow that deleted proof just because the endpoints live.
    f.state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(witness))
        .await
        .unwrap();
    let path = format!(
        "/api/v1/trace/agents/{}/origins?at_snapshot={token}",
        recipient.encode()
    );
    let (status, page) = f.get(&path).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "one independent proof survives: {page}");
    assert_eq!(items[0]["origin"]["name"], "Original");
    let proof = ClaimRef::decode(items[0]["claim"].as_str().unwrap()).unwrap();
    assert!(
        matches!(proof.address(),ObjectAddress::Native {key,..} if key==&vec!["local-origin".to_string()])
    );
    let (status, claim) = f
        .get(&format!(
            "/api/v1/trace/objects/{}/claims/{}?at_snapshot={token}",
            recipient.encode(),
            proof.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{claim}");
    let (status, _) = f.get(&format!("{path}&selection=not-a-selection")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn origin_pages_are_bounded_and_cursors_cannot_cross_recipients() {
    let mut f = ProvenanceFixture::new().await;
    let recipient = agent("recipient", "many-origins");
    let other = agent("recipient", "other-origins");
    let capture = f
        .capture(&recipient, "many", b"retained origin assertions")
        .await;
    let count = f.state.trace.limits().max_items + 11;
    let mut claims = vec![
        claim(
            &recipient,
            "identity",
            agent_object(&recipient, "Fork"),
            &[&capture],
        ),
        claim(
            &other,
            "identity",
            agent_object(&other, "Other"),
            &[&capture],
        ),
    ];
    for index in 0..count {
        let mut proof = claim(
            &recipient,
            &format!("proof-{index:04}"),
            agent_object(&recipient, "unused"),
            &[&capture],
        );
        proof.fact = Fact::Relation {
            relation: Relation::ContextLineage {
                recipient: recipient.clone(),
                checkpoint: None,
                original_owner: OwnerRef::Unknown,
            },
        };
        claims.push(proof);
    }
    f.publish(claims, &[&capture]).await;
    let boot = f.bootstrap(&recipient).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let (status, first) = f
        .get(&format!(
            "/api/v1/trace/agents/{}/origins?at_snapshot={token}",
            recipient.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let limit = first["limits"]["maxItems"].as_u64().unwrap() as usize;
    assert_eq!(first["items"].as_array().unwrap().len(), limit);
    assert!(
        first["returnedBytes"].as_u64().unwrap()
            <= first["limits"]["maxSummaryBytes"].as_u64().unwrap()
    );
    let cursor = first["afterCursor"].as_str().unwrap();
    let (status, _) = f
        .get(&format!(
            "/api/v1/trace/agents/{}/origins?at_snapshot={token}&after={cursor}",
            other.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, last) = f
        .get(&format!(
            "/api/v1/trace/agents/{}/origins?at_snapshot={token}&after={cursor}",
            recipient.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{last}");
    assert_eq!(last["items"].as_array().unwrap().len(), count - limit);
    assert_eq!(last["hasAfter"], false);
}
