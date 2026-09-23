use super::*;

const CLAUDE_FORK: &[u8] = include_bytes!(
    "../../../../../../../fixtures/native/claude/fork-replay-messageuuid-mirrors-own-uuid.jsonl"
);
fn fork() -> AgentRef {
    AgentRef::Session(
        SessionId::decode("env7fk2:claude:f9dc6c53-da48-4d6f-9c68-b9a1708ad6dc").unwrap(),
    )
}
fn origin() -> AgentRef {
    AgentRef::Session(
        SessionId::decode("env7fk2:claude:bba7f74d-801e-4a61-b814-3091cafeb904").unwrap(),
    )
}
async fn assert_lineage(store: &GenerationStore) {
    let claims = claims(store).await;
    let lineage: Vec<_> = claims
        .iter()
        .filter_map(|claim| match &claim.fact {
            Fact::Relation {
                relation:
                    Relation::ContextLineage {
                        recipient,
                        checkpoint,
                        original_owner,
                    },
            } => Some((recipient, checkpoint, original_owner)),
            _ => None,
        })
        .collect();
    assert_eq!(
        lineage.len(),
        10,
        "each observed copied-record stamp must survive admission"
    );
    for (recipient, checkpoint, original) in lineage {
        assert_eq!(recipient, &fork());
        assert_eq!(checkpoint, &None);
        assert_eq!(original, &OwnerRef::Agent { agent: origin() });
    }
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let row = store
        .read_page(
            &snapshot,
            &read_scope(&fork(), None, "header"),
            None,
            None,
            None,
            1,
            false,
        )
        .await
        .unwrap();
    let header: AgentHeader = serde_json::from_value(row.rows[0].body.clone()).unwrap();
    assert_eq!(header.origin.as_ref().unwrap().address.agent(), origin());
    assert!(header.parent.is_none());
    assert_eq!(header.descendant_count.value, Some(0));
}

#[tokio::test]
async fn recorded_claude_fork_lineage_survives_restart_reread_and_same_cut_rebuild() {
    let mut expected = None;
    for restarted in [false, true] {
        let fixture = Fixture::for_vendor("claude").await;
        let (mut store, _) = fixture.generation("fork-lineage").await;
        let mut captures = vec![];
        for record in source_records(CLAUDE_FORK, b"", SourceOrder::NativeFirst) {
            captures.push(admit_record(&fixture, &record).await);
            if restarted {
                fixture
                    .worker()
                    .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                    .await
                    .unwrap();
                store = reopened(store).await;
            }
        }
        let cut = fixture.evidence.evidence_cut().await.unwrap();
        fixture.worker().catch_up(&store, 1, cut).await.unwrap();
        assert_lineage(&store).await;
        let before = projection(&fixture, &store).await;
        for mut capture in captures {
            capture.observation_ref.producer_id = "independent-native-reread".into();
            fixture
                .evidence
                .admit_batch(
                    &capture.observation_ref.issuer,
                    std::slice::from_ref(&capture),
                )
                .await
                .unwrap();
        }
        fixture
            .worker()
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        assert_lineage(&store).await;
        assert_eq!(projection(&fixture, &store).await, before);
        let (rebuilt, _) = fixture.generation("fork-rebuilt").await;
        fixture.worker().catch_up(&rebuilt, 1, cut).await.unwrap();
        assert_lineage(&rebuilt).await;
        assert_eq!(projection(&fixture, &rebuilt).await, before);
        let normalized = Projection {
            claims: before.claims,
            reader: before
                .reader
                .normalize_separate_archive_epochs(&fixture.catalogue)
                .await,
        };
        if let Some(expected) = &expected {
            assert_eq!(&normalized, expected);
        } else {
            expected = Some(normalized);
        }
    }
}
