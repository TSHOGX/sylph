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
