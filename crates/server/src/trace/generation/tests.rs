use super::*;
use sylph_protocol::{
    SessionId,
    trace::{
        AgentThread, ClaimRef, EntryRef, Fact, HistoryEntry, Issuer, ObservationRef, OwnerRef,
    },
};

struct Fixture {
    dir: tempfile::TempDir,
    store: GenerationStore,
    agent: AgentRef,
    selection: SelectionRef,
}

#[tokio::test]
async fn projection_writer_discards_an_acknowledged_begin_without_a_transaction_guard() {
    let fixture = Fixture::new().await;
    crate::db::sqlite::assert_abandoned_begin_is_discarded(&fixture.store.writer).await;
}

impl Fixture {
    async fn new() -> Self {
        Self::with_limits(QueryLimits::default()).await
    }

    async fn with_limits(limits: QueryLimits) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:root").unwrap());
        let selection =
            SelectionRef::native(agent.clone(), "history", vec!["selection".into()]).unwrap();
        let manifest = GenerationManifest {
            generation: "generation-a".into(),
            semantic_schema: "trace-1".into(),
            mapper_builds: BTreeMap::from([("codex".into(), "codex-mapper-1".into())]),
            reducer_build: "reducer-1".into(),
            capture_inventory_build: "inventory-1".into(),
            encoding_build: "encoding-1".into(),
        };
        let store = GenerationStore::create(dir.path().join("trace-a.db"), manifest, limits)
            .await
            .unwrap();
        Self {
            dir,
            store,
            agent,
            selection,
        }
    }

    fn scope(&self) -> QueryScope {
        QueryScope::history(self.agent.clone(), self.selection.clone())
    }

    fn entry(&self, id: &str) -> TraceObject {
        TraceObject::HistoryEntry(HistoryEntry {
            entry_ref: EntryRef::native(self.agent.clone(), "entries", vec![id.into()]).unwrap(),
            containing_agent: self.agent.clone(),
            predecessor: None,
            original_owner: None,
        })
    }

    fn member(&self, object: &TraceObject, order: u64) -> MembershipChange {
        MembershipChange {
            scope: self.scope(),
            object_ref: object.object_ref(),
            order_key: order.to_be_bytes().to_vec(),
            present: true,
        }
    }

    async fn publish(&self, batch: ProjectionBatch) -> Snapshot {
        let prior = self.store.checkpoint().await.unwrap();
        self.store
            .apply(prior.fence_token, &prior.snapshot, &batch)
            .await
            .unwrap()
            .snapshot
    }

    async fn reopen(&mut self) {
        self.store.close().await;
        self.store = GenerationStore::open(
            self.dir.path().join("trace-a.db"),
            self.store.manifest().clone(),
            self.store.limits().clone(),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn late_order_and_body_changes_do_not_omit_or_duplicate_old_snapshot_members() {
    let fixture = Fixture::new().await;
    let a = fixture.entry("a");
    let b = fixture.entry("b");
    let c = fixture.entry("c");
    let old = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![
                ObjectUpdate::put(a.clone(), "A original"),
                ObjectUpdate::put(b.clone(), "B original"),
                ObjectUpdate::put(c.clone(), "C original"),
            ],
            memberships: vec![
                fixture.member(&a, 10),
                fixture.member(&b, 20),
                fixture.member(&c, 30),
            ],
            ..Default::default()
        })
        .await;
    let first = fixture
        .store
        .page(&old, &fixture.scope(), None, 2)
        .await
        .unwrap();
    assert_eq!(
        first
            .objects
            .iter()
            .map(|item| item.preview.as_str())
            .collect::<Vec<_>>(),
        ["A original", "B original"]
    );
    let mut changed_b = b.clone();
    if let TraceObject::HistoryEntry(entry) = &mut changed_b {
        entry.predecessor =
            Some(EntryRef::native(fixture.agent.clone(), "entries", vec!["a".into()]).unwrap());
    }
    let new = fixture
        .publish(ProjectionBatch {
            through_cut: 2,
            objects: vec![ObjectUpdate::put(changed_b.clone(), "B corrected")],
            memberships: vec![fixture.member(&c, 5)],
            ..Default::default()
        })
        .await;
    let second = fixture
        .store
        .page(&old, &fixture.scope(), first.next_cursor.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(
        second
            .objects
            .iter()
            .map(|item| item.preview.as_str())
            .collect::<Vec<_>>(),
        ["C original"]
    );
    assert_eq!(
        fixture
            .store
            .object(&old, &b.object_ref())
            .await
            .unwrap()
            .object,
        b
    );
    assert_eq!(
        fixture
            .store
            .object(&new, &changed_b.object_ref())
            .await
            .unwrap()
            .object,
        changed_b
    );
    let current = fixture
        .store
        .page(&new, &fixture.scope(), None, 3)
        .await
        .unwrap();
    assert_eq!(
        current
            .objects
            .iter()
            .map(|item| item.preview.as_str())
            .collect::<Vec<_>>(),
        ["C original", "A original", "B corrected"]
    );
    assert_eq!(current.total_count, 3);
}

#[tokio::test]
async fn summary_revisions_do_not_multiply_history_membership_rows() {
    let fixture = Fixture::new().await;
    let entry = fixture.entry("stream");
    let old = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![ObjectUpdate::put(entry.clone(), "revision one")],
            memberships: vec![fixture.member(&entry, 1)],
            ..Default::default()
        })
        .await;
    for revision in 2..=100 {
        fixture
            .publish(ProjectionBatch {
                through_cut: revision,
                objects: vec![ObjectUpdate::put(
                    entry.clone(),
                    format!("revision {revision}"),
                )],
                ..Default::default()
            })
            .await;
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memberships")
        .fetch_one(&fixture.store.readers)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        fixture
            .store
            .summary(&old, &entry.object_ref())
            .await
            .unwrap()
            .preview,
        "revision one"
    );
    let current = fixture.store.checkpoint().await.unwrap().snapshot;
    let page = fixture
        .store
        .page(&current, &fixture.scope(), None, 1)
        .await
        .unwrap();
    assert_eq!(page.objects[0].preview, "revision 100");
    assert_eq!(page.work.candidate_rows, 1);
}

#[tokio::test]
async fn exhausted_candidate_work_returns_an_advancing_cursor_even_without_visible_rows() {
    let fixture = Fixture::with_limits(QueryLimits {
        max_candidates: 2,
        ..Default::default()
    })
    .await;
    let entry = fixture.entry("moving");
    fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![ObjectUpdate::put(entry.clone(), "one entry")],
            memberships: vec![fixture.member(&entry, 1)],
            ..Default::default()
        })
        .await;
    for cut in 2..=8 {
        fixture
            .publish(ProjectionBatch {
                through_cut: cut,
                memberships: vec![fixture.member(&entry, cut)],
                ..Default::default()
            })
            .await;
    }
    let current = fixture.store.checkpoint().await.unwrap().snapshot;
    let mut cursor = None;
    let mut seen = Vec::new();
    let mut empty_budget_pages = 0;
    loop {
        let page = fixture
            .store
            .page(&current, &fixture.scope(), cursor.as_deref(), 1)
            .await
            .unwrap();
        assert_eq!(page.total_count, 1);
        assert!(page.work.candidate_rows <= fixture.store.limits().max_candidates);
        if page.objects.is_empty() {
            assert_eq!(page.state, PageState::WorkBudgetExhausted);
            assert!(page.has_more);
            empty_budget_pages += 1;
        }
        seen.extend(page.objects);
        if !page.has_more {
            break;
        }
        assert_ne!(cursor, page.next_cursor);
        cursor = page.next_cursor;
    }
    assert_eq!(empty_budget_pages, 3);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].object_ref, entry.object_ref());
}

#[tokio::test]
async fn reusable_snapshots_keep_scope_cursors_bound_to_every_query_option() {
    let fixture = Fixture::new().await;
    let a = fixture.entry("a");
    let b = fixture.entry("b");
    let agent = TraceObject::Agent(AgentThread {
        agent_ref: fixture.agent.clone(),
        vendor: fixture.agent.anchor_session().agent.clone(),
        name: Some("Root".into()),
        model: None,
    });
    let snapshot = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![
                ObjectUpdate::put(a.clone(), "a"),
                ObjectUpdate::put(b.clone(), "b"),
                ObjectUpdate::put(agent.clone(), "Root"),
            ],
            memberships: vec![fixture.member(&a, 1), fixture.member(&b, 2)],
            ..Default::default()
        })
        .await;
    let first = fixture
        .store
        .page(&snapshot, &fixture.scope(), None, 1)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .store
            .object(&snapshot, &agent.object_ref())
            .await
            .unwrap()
            .snapshot,
        snapshot
    );
    assert_eq!(
        fixture
            .store
            .object(&snapshot, &a.object_ref())
            .await
            .unwrap()
            .snapshot,
        snapshot
    );
    let mut variants = Vec::new();
    let mut scope = fixture.scope();
    scope.history_mode = HistoryMode::Authored;
    variants.push(scope);
    let mut scope = fixture.scope();
    scope.representation = Some(Representation::Runtime);
    variants.push(scope);
    let mut scope = fixture.scope();
    scope.filter = MembershipFilter::Active;
    variants.push(scope);
    variants.push(QueryScope::history(
        fixture.agent.clone(),
        SelectionRef::native(fixture.agent.clone(), "history", vec!["another".into()]).unwrap(),
    ));
    for scope in variants {
        assert!(matches!(
            fixture
                .store
                .page(&snapshot, &scope, first.next_cursor.as_deref(), 1)
                .await,
            Err(GenerationError::InvalidCursor)
        ));
    }
    let mut wrong = snapshot.clone();
    wrong.evidence_cut += 1;
    assert!(matches!(
        fixture.store.summary(&wrong, &a.object_ref()).await,
        Err(GenerationError::InvalidSnapshot)
    ));
    assert!(matches!(
        fixture
            .store
            .page(&snapshot, &fixture.scope(), Some("not base64!"), 1)
            .await,
        Err(GenerationError::InvalidCursor)
    ));
}

#[tokio::test]
async fn payload_authorization_requires_the_exact_anchor_revision_recipient_and_retained_range() {
    let fixture = Fixture::new().await;
    let recipient = AgentRef::Session(SessionId::decode("env7fk3:codex:recipient").unwrap());
    let entry = TraceObject::HistoryEntry(HistoryEntry {
        entry_ref: EntryRef::native(recipient.clone(), "copied", vec!["entry".into()]).unwrap(),
        containing_agent: recipient.clone(),
        predecessor: None,
        original_owner: Some(OwnerRef::Agent {
            agent: fixture.agent.clone(),
        }),
    });
    let p1 = PayloadAnchor {
        via_ref: entry.object_ref(),
        containing_agent: recipient.clone(),
        selection: Some(fixture.selection.clone()),
        payload_ref: PayloadRef::bytes("1".repeat(64)).unwrap(),
        body_revision: "R1".into(),
        dependency_epochs: BTreeSet::new(),
        representation: Representation::Conversation,
        ranges: vec![PayloadRange {
            start: 4,
            end_exclusive: 12,
        }],
    };
    let old = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![ObjectUpdate::put(entry.clone(), "copied bytes")],
            payload_anchors: vec![PayloadAnchorChange {
                anchor: p1.clone(),
                present: true,
            }],
            ..Default::default()
        })
        .await;
    let mut p2 = p1.clone();
    p2.payload_ref = PayloadRef::bytes("2".repeat(64)).unwrap();
    p2.body_revision = "R2".into();
    let new = fixture
        .publish(ProjectionBatch {
            through_cut: 2,
            payload_anchors: vec![
                PayloadAnchorChange {
                    anchor: p1.clone(),
                    present: false,
                },
                PayloadAnchorChange {
                    anchor: p2.clone(),
                    present: true,
                },
            ],
            ..Default::default()
        })
        .await;
    fixture
        .store
        .authorize_payload(&old, &p1, 5, 10)
        .await
        .unwrap();
    assert!(matches!(
        fixture.store.authorize_payload(&old, &p2, 5, 10).await,
        Err(GenerationError::PayloadNotReachable)
    ));
    assert!(matches!(
        fixture.store.authorize_payload(&new, &p1, 5, 10).await,
        Err(GenerationError::PayloadNotReachable)
    ));
    assert!(matches!(
        fixture.store.authorize_payload(&old, &p1, 0, 10).await,
        Err(GenerationError::PayloadNotReachable)
    ));
    let mut wrong_recipient = p1.clone();
    wrong_recipient.containing_agent = fixture.agent.clone();
    assert!(matches!(
        fixture
            .store
            .authorize_payload(&old, &wrong_recipient, 5, 10)
            .await,
        Err(GenerationError::PayloadNotReachable)
    ));
    fixture
        .store
        .authorize_payload(&new, &p2, 5, 10)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_fence_survives_reopen_and_rejects_every_queued_writer_with_an_old_token() {
    let mut fixture = Fixture::new().await;
    let entry = fixture.entry("a");
    let snapshot = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![ObjectUpdate::put(entry.clone(), "before fence")],
            ..Default::default()
        })
        .await;
    let fence = fixture.store.fence(1).await.unwrap();
    assert_eq!(fence.checkpoint, snapshot);
    fixture.reopen().await;
    let batch = ProjectionBatch {
        through_cut: 2,
        objects: vec![ObjectUpdate::put(entry.clone(), "after fence")],
        ..Default::default()
    };
    assert!(matches!(
        fixture.store.apply(1, &snapshot, &batch).await,
        Err(GenerationError::Fenced)
    ));
    assert!(matches!(
        fixture.store.apply(fence.token, &snapshot, &batch).await,
        Err(GenerationError::Fenced)
    ));
    let token = fixture.store.resume(&fence).await.unwrap();
    assert!(matches!(
        fixture.store.apply(1, &snapshot, &batch).await,
        Err(GenerationError::Fenced)
    ));
    fixture.store.apply(token, &snapshot, &batch).await.unwrap();
    assert!(matches!(
        fixture.store.resume(&fence).await,
        Err(GenerationError::Fenced)
    ));
}

#[tokio::test]
async fn an_in_flight_writer_commits_before_the_fence_samples_its_cut() {
    let fixture = Fixture::new().await;
    let other_writer = GenerationStore::open(
        fixture.store.path(),
        fixture.store.manifest().clone(),
        fixture.store.limits().clone(),
    )
    .await
    .unwrap();
    let mut tx = fixture
        .store
        .writer
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    sqlx::query("INSERT INTO projection_revisions VALUES (1, 10)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE projection_state SET evidence_cut = 10, view_revision = 1 WHERE singleton = 1",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let fence_task = {
        let entered = entered.clone();
        tokio::spawn(async move { other_writer.fence_inner(1, Some(&entered)).await.unwrap() })
    };
    entered.notified().await;
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(!fence_task.is_finished());
    tx.commit().await.unwrap();
    let fence = fence_task.await.unwrap();
    assert_eq!(fence.checkpoint.evidence_cut, 10);
    assert_eq!(fence.checkpoint.view_revision, 1);
    assert!(matches!(
        fixture
            .store
            .apply(
                1,
                &fence.checkpoint,
                &ProjectionBatch {
                    through_cut: 11,
                    ..Default::default()
                }
            )
            .await,
        Err(GenerationError::Fenced)
    ));
}

#[tokio::test]
async fn a_failed_projection_unit_rolls_back_objects_claims_memberships_and_checkpoint() {
    let fixture = Fixture::new().await;
    let object = fixture.entry("a");
    let claim = Claim {
        claim_ref: ClaimRef::native(fixture.agent.clone(), "claim", vec!["one".into()]).unwrap(),
        fact: Fact::Declare {
            object: object.clone(),
        },
        evidence: vec![ObservationRef {
            issuer: Issuer::Environment(fixture.agent.environment().clone()),
            producer_id: "p".into(),
            producer_epoch: "e".into(),
            producer_seq: 1,
        }],
        rule_id: "rule".into(),
        mapper_build: "mapper".into(),
        supersedes: vec![],
        source_revision: None,
    };
    let absent = fixture.entry("absent");
    let failed = ProjectionBatch {
        through_cut: 1,
        claims: vec![claim.clone()],
        objects: vec![ObjectUpdate::put(object.clone(), "a")],
        memberships: vec![fixture.member(&object, 1), fixture.member(&absent, 2)],
        ..Default::default()
    };
    assert!(
        fixture
            .store
            .apply(
                1,
                &fixture.store.checkpoint().await.unwrap().snapshot,
                &failed,
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .checkpoint()
            .await
            .unwrap()
            .snapshot
            .evidence_cut,
        0
    );
    let snapshot = Snapshot {
        generation: "generation-a".into(),
        evidence_cut: 0,
        view_revision: 0,
    };
    assert!(matches!(
        fixture.store.summary(&snapshot, &object.object_ref()).await,
        Err(GenerationError::NotFound)
    ));
    let claims: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM semantic_claims")
        .fetch_one(&fixture.store.readers)
        .await
        .unwrap();
    assert_eq!(claims, 0);
    let first = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            claims: vec![claim.clone()],
            objects: vec![ObjectUpdate::put(object.clone(), "a")],
            memberships: vec![fixture.member(&object, 1)],
            ..Default::default()
        })
        .await;
    let mut collision = claim;
    collision.rule_id = "wrong".into();
    assert!(matches!(
        fixture
            .store
            .apply(
                1,
                &first,
                &ProjectionBatch {
                    through_cut: 2,
                    claims: vec![collision],
                    ..Default::default()
                }
            )
            .await,
        Err(GenerationError::ClaimCollision)
    ));
    assert_eq!(fixture.store.checkpoint().await.unwrap().snapshot, first);
}

#[tokio::test]
async fn withdrawal_is_distinct_from_absence_and_preserves_old_object_snapshots() {
    let fixture = Fixture::new().await;
    let entry = fixture.entry("withdrawn");
    let old = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![ObjectUpdate::put(entry.clone(), "present")],
            ..Default::default()
        })
        .await;
    let new = fixture
        .publish(ProjectionBatch {
            through_cut: 2,
            objects: vec![ObjectUpdate::Withdraw {
                object_ref: entry.object_ref(),
                reason: "equivalence proof was withdrawn".into(),
            }],
            ..Default::default()
        })
        .await;
    assert_eq!(
        fixture
            .store
            .object(&old, &entry.object_ref())
            .await
            .unwrap()
            .object,
        entry
    );
    assert!(
        matches!(fixture.store.object(&new, &entry.object_ref()).await, Err(GenerationError::InterpretationWithdrawn(reason)) if reason == "equivalence proof was withdrawn")
    );
    assert!(matches!(
        fixture
            .store
            .object(&new, &fixture.entry("absent").object_ref())
            .await,
        Err(GenerationError::NotFound)
    ));
    let changes = fixture.store.changes_after(1, 10).await.unwrap();
    assert_eq!(changes[0].removed, vec![entry.object_ref()]);
    assert_eq!(changes[0].snapshot, new);
}

#[tokio::test]
async fn sealed_generation_identity_and_builds_survive_reopen_before_publication() {
    let mut fixture = Fixture::new().await;
    let validation = fixture.store.validate_for_publication().await.unwrap();
    let fence = fixture.store.fence(1).await.unwrap();
    let identity = fixture.store.seal(&fence, &validation).await.unwrap();
    fixture.reopen().await;
    assert_eq!(
        fixture.store.checkpoint().await.unwrap().writer_mode,
        WriterMode::Sealed
    );
    let stored: String = sqlx::query_scalar("SELECT database_identity FROM generation_manifest")
        .fetch_one(&fixture.store.readers)
        .await
        .unwrap();
    assert_eq!(stored, identity);
    let mut changed = fixture.store.manifest().clone();
    changed.reducer_build = "another-build".into();
    assert!(matches!(
        GenerationStore::open(
            fixture.store.path(),
            changed,
            fixture.store.limits().clone()
        )
        .await,
        Err(GenerationError::ManifestMismatch)
    ));
    fixture.store.resume(&fence).await.unwrap();
}

#[tokio::test]
async fn expired_revision_floor_is_explicit_and_cannot_regress_after_restart() {
    let mut fixture = Fixture::new().await;
    let entry = fixture.entry("old");
    let old = fixture
        .publish(ProjectionBatch {
            through_cut: 4,
            objects: vec![ObjectUpdate::put(entry.clone(), "original")],
            ..Default::default()
        })
        .await;
    let current = fixture
        .publish(ProjectionBatch {
            through_cut: 9,
            objects: vec![ObjectUpdate::put(entry.clone(), "current")],
            ..Default::default()
        })
        .await;
    fixture
        .store
        .advance_revision_floor(current.view_revision)
        .await
        .unwrap();
    fixture.reopen().await;
    assert!(matches!(
        fixture.store.object(&old, &entry.object_ref()).await,
        Err(GenerationError::SnapshotExpired)
    ));
    assert_eq!(
        fixture
            .store
            .object(&current, &entry.object_ref())
            .await
            .unwrap()
            .summary
            .preview,
        "current"
    );
    assert!(
        fixture
            .store
            .advance_revision_floor(old.view_revision)
            .await
            .is_err()
    );
    assert!(
        fixture
            .store
            .advance_revision_floor(current.view_revision + 1)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn seal_rejects_validation_before_a_later_committed_projection_cut() {
    let fixture = Fixture::new().await;
    let validation = fixture.store.validate_for_publication().await.unwrap();
    let entry = fixture.entry("later");
    fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![ObjectUpdate::put(entry, "later")],
            ..Default::default()
        })
        .await;
    let fence = fixture.store.fence(1).await.unwrap();
    assert!(fixture.store.seal(&fence, &validation).await.is_err());
    assert_eq!(
        fixture.store.checkpoint().await.unwrap().writer_mode,
        WriterMode::Fenced
    );
    let complete = fixture.store.validate_for_publication().await.unwrap();
    fixture.store.seal(&fence, &complete).await.unwrap();
}

#[tokio::test]
async fn summary_byte_limit_stops_before_an_extra_object_and_keeps_its_cursor() {
    let mut fixture = Fixture::new().await;
    let a = fixture.entry("a");
    let b = fixture.entry("b");
    let snapshot = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![
                ObjectUpdate::put(a.clone(), "one"),
                ObjectUpdate::put(b.clone(), "two"),
            ],
            memberships: vec![fixture.member(&a, 1), fixture.member(&b, 2)],
            ..Default::default()
        })
        .await;
    let summary = fixture
        .store
        .summary(&snapshot, &a.object_ref())
        .await
        .unwrap();
    fixture.store.limits.max_summary_bytes = serde_json::to_vec(&summary).unwrap().len();
    let first = fixture
        .store
        .page(&snapshot, &fixture.scope(), None, 2)
        .await
        .unwrap();
    assert_eq!(first.returned_count, 1);
    assert!(first.returned_bytes <= first.limits.max_summary_bytes);
    assert_eq!(first.state, PageState::More);
    let second = fixture
        .store
        .page(&snapshot, &fixture.scope(), first.next_cursor.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(second.objects[0].object_ref, b.object_ref());
    assert_eq!(second.state, PageState::Complete);
}

#[tokio::test]
async fn query_vm_budget_interrupts_work_explicitly_and_does_not_poison_reused_connections() {
    let mut fixture = Fixture::new().await;
    let a = fixture.entry("a");
    let snapshot = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: vec![ObjectUpdate::put(a.clone(), "a")],
            memberships: vec![fixture.member(&a, 1)],
            ..Default::default()
        })
        .await;
    let configured = fixture.store.limits.max_vm_steps;
    fixture.store.limits.max_vm_steps = 1;
    assert!(matches!(
        fixture
            .store
            .page(&snapshot, &fixture.scope(), None, 1)
            .await,
        Err(GenerationError::WorkBudget)
    ));
    fixture.store.limits.max_vm_steps = configured;
    let page = fixture
        .store
        .page(&snapshot, &fixture.scope(), None, 1)
        .await
        .unwrap();
    assert_eq!(page.objects[0].object_ref, a.object_ref());
    assert!(page.work.vm_steps > 1);
    assert!(page.work.vm_steps <= page.limits.max_vm_steps);
}

#[tokio::test]
async fn an_object_anchor_seeks_its_selected_history_position_without_loading_the_prefix() {
    let fixture = Fixture::with_limits(QueryLimits {
        max_candidates: 2,
        ..Default::default()
    })
    .await;
    let entries: Vec<_> = (0..20)
        .map(|n| fixture.entry(&format!("item-{n:02}")))
        .collect();
    let snapshot = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            objects: entries
                .iter()
                .map(|entry| ObjectUpdate::put(entry.clone(), "entry"))
                .collect(),
            memberships: entries
                .iter()
                .enumerate()
                .map(|(i, entry)| fixture.member(entry, i as u64))
                .collect(),
            ..Default::default()
        })
        .await;
    let page = fixture
        .store
        .page_at_object(&snapshot, &fixture.scope(), &entries[17].object_ref(), 1)
        .await
        .unwrap();
    assert_eq!(page.objects[0].object_ref, entries[17].object_ref());
    assert_eq!(page.work.candidate_rows, 1);
    let mut removal = fixture.member(&entries[17], 17);
    removal.present = false;
    let current = fixture
        .publish(ProjectionBatch {
            through_cut: 2,
            memberships: vec![removal],
            ..Default::default()
        })
        .await;
    assert!(matches!(
        fixture
            .store
            .page_at_object(&current, &fixture.scope(), &entries[17].object_ref(), 1)
            .await,
        Err(GenerationError::AnchorOutsideSelection)
    ));
    assert_eq!(
        fixture
            .store
            .page_at_object(&snapshot, &fixture.scope(), &entries[17].object_ref(), 1)
            .await
            .unwrap()
            .objects[0]
            .object_ref,
        entries[17].object_ref()
    );
}

#[tokio::test]
async fn claim_pages_index_every_typed_participant_and_keep_filters_and_evidence_at_the_snapshot() {
    use sylph_protocol::trace::Relation;

    let fixture = Fixture::new().await;
    let child = AgentRef::Embedded {
        anchor_session: fixture.agent.anchor_session().clone(),
        namespace: "children".into(),
        native_child_id: "child-b".into(),
    };
    let receipt = ObservationRef {
        issuer: Issuer::Environment(fixture.agent.environment().clone()),
        producer_id: "capture".into(),
        producer_epoch: "incarnation".into(),
        producer_seq: 1,
    };
    let spawn = Claim {
        claim_ref: ClaimRef::native(fixture.agent.clone(), "claims", vec!["spawn".into()]).unwrap(),
        fact: Fact::Relation {
            relation: Relation::Spawn {
                parent: fixture.agent.clone(),
                child: child.clone(),
                operation: None,
            },
        },
        evidence: vec![receipt.clone()],
        rule_id: "observed_spawn_result".into(),
        mapper_build: "codex-test".into(),
        supersedes: vec![],
        source_revision: None,
    };
    let input = Claim {
        claim_ref: ClaimRef::native(fixture.agent.clone(), "claims", vec!["exists".into()])
            .unwrap(),
        fact: Fact::AgentState {
            agent: fixture.agent.clone(),
            state: sylph_protocol::trace::AgentState::Exists,
        },
        evidence: vec![receipt.clone(), receipt.clone()],
        rule_id: "native_header".into(),
        mapper_build: "codex-test".into(),
        supersedes: vec![],
        source_revision: None,
    };
    let old = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            claims: vec![spawn.clone(), input.clone()],
            ..Default::default()
        })
        .await;
    let child_claims = fixture
        .store
        .claims_for(&old, &AnyObjectRef::Agent(child.clone()), None, None, 10)
        .await
        .unwrap();
    assert_eq!(child_claims.claims.len(), 1);
    assert_eq!(child_claims.claims[0].claim_ref, spawn.claim_ref);
    assert_eq!(child_claims.claims[0].fact_kind, TraceFactKind::Relation);
    let parent = AnyObjectRef::Agent(fixture.agent.clone());
    let first = fixture
        .store
        .claims_for(&old, &parent, None, None, 1)
        .await
        .unwrap();
    assert!(first.has_more);
    let second = fixture
        .store
        .claims_for(&old, &parent, None, first.next_cursor.as_deref(), 1)
        .await
        .unwrap();
    let found: BTreeSet<_> = first
        .claims
        .iter()
        .chain(second.claims.iter())
        .map(|claim| claim.claim_ref.clone())
        .collect();
    assert_eq!(
        found,
        BTreeSet::from([spawn.claim_ref.clone(), input.claim_ref.clone()])
    );
    assert!(matches!(
        fixture
            .store
            .claims_for(
                &old,
                &AnyObjectRef::Agent(child),
                None,
                first.next_cursor.as_deref(),
                1
            )
            .await,
        Err(GenerationError::InvalidCursor)
    ));
    assert!(matches!(
        fixture
            .store
            .claims_for(
                &old,
                &parent,
                Some(TraceFactKind::Relation),
                first.next_cursor.as_deref(),
                1
            )
            .await,
        Err(GenerationError::InvalidCursor)
    ));
    let filtered = fixture
        .store
        .claims_for(&old, &parent, Some(TraceFactKind::AgentState), None, 10)
        .await
        .unwrap();
    assert_eq!(filtered.claims[0].rule_id, "native_header");
    assert_eq!(filtered.claims[0].evidence_count, 1);
    assert_eq!(
        fixture
            .store
            .claim(&old, &input.claim_ref)
            .await
            .unwrap()
            .evidence,
        [receipt.clone(), receipt]
    );
    let mut later = input.clone();
    later.claim_ref =
        ClaimRef::native(fixture.agent.clone(), "claims", vec!["later".into()]).unwrap();
    later.supersedes.push(input.claim_ref.clone());
    let current = fixture
        .publish(ProjectionBatch {
            through_cut: 2,
            claims: vec![later.clone()],
            ..Default::default()
        })
        .await;
    assert_eq!(
        fixture
            .store
            .claims_for(&old, &parent, None, None, 10)
            .await
            .unwrap()
            .claims
            .len(),
        2
    );
    assert_eq!(
        fixture
            .store
            .claims_for(&current, &parent, None, None, 10)
            .await
            .unwrap()
            .claims
            .len(),
        3
    );
    assert!(matches!(
        fixture.store.claim(&old, &later.claim_ref).await,
        Err(GenerationError::NotFound)
    ));
    assert_eq!(
        fixture
            .store
            .claim(&current, &later.claim_ref)
            .await
            .unwrap()
            .supersedes,
        [input.claim_ref]
    );
}

#[tokio::test]
async fn foreground_work_stays_bounded_when_history_and_invalid_version_density_grow_tenfold() {
    let mut measurements = Vec::new();
    for history in [100, 1000] {
        for density in [1u64, 10, 100] {
            let fixture = Fixture::with_limits(QueryLimits {
                max_candidates: 32,
                max_batch_changes: 4000,
                max_batch_bytes: 16 * 1024 * 1024,
                ..Default::default()
            })
            .await;
            let entries: Vec<_> = (0..history)
                .map(|n| fixture.entry(&format!("entry-{n:06}")))
                .collect();
            fixture
                .publish(ProjectionBatch {
                    through_cut: 1,
                    objects: entries
                        .iter()
                        .map(|entry| ObjectUpdate::put(entry.clone(), "bounded preview"))
                        .collect(),
                    memberships: entries
                        .iter()
                        .enumerate()
                        .map(|(i, entry)| fixture.member(entry, i as u64))
                        .collect(),
                    ..Default::default()
                })
                .await;
            if density > 1 {
                // This performance fixture varies physical version density without
                // confusing it with mapper CPU work or source payload decoding.
                let mut tx = fixture
                    .store
                    .writer
                    .begin_with("BEGIN IMMEDIATE")
                    .await
                    .unwrap();
                for revision in 2..=density {
                    sqlx::query("INSERT INTO projection_revisions VALUES (?, ?)")
                        .bind(revision as i64)
                        .bind(revision as i64)
                        .execute(&mut *tx)
                        .await
                        .unwrap();
                }
                sqlx::query("UPDATE memberships SET valid_to = 2 WHERE valid_from = 1")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                for revision in 2..=density {
                    sqlx::query("INSERT INTO memberships (scope_key, order_key, object_ref, valid_from, valid_to) SELECT scope_key, order_key, object_ref, ?, ? FROM memberships WHERE valid_from = 1")
                        .bind(revision as i64).bind((revision < density).then_some(revision as i64 + 1)).execute(&mut *tx).await.unwrap();
                }
                sqlx::query("UPDATE projection_state SET view_revision = ?, evidence_cut = ?")
                    .bind(density as i64)
                    .bind(density as i64)
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            }
            let snapshot = fixture.store.checkpoint().await.unwrap().snapshot;
            let page = fixture
                .store
                .page(&snapshot, &fixture.scope(), None, 4)
                .await
                .unwrap();
            assert_eq!(page.total_count, history as u64);
            assert!(page.work.candidate_rows <= page.limits.max_candidates);
            assert!(
                page.work.vm_steps < 5000,
                "history={history}, density={density}: {} VM steps",
                page.work.vm_steps
            );
            if density > page.limits.max_candidates as u64 {
                assert_eq!(page.state, PageState::WorkBudgetExhausted);
                assert_eq!(page.returned_count, 0);
                assert!(page.next_cursor.is_some());
            }
            measurements.push((
                history,
                density,
                page.work.candidate_rows,
                page.work.vm_steps,
            ));
        }
    }
    for (history, density, candidates, vm_steps) in &measurements {
        eprintln!(
            "history={history}, superseded-density={density}, candidates={candidates}, VM steps={vm_steps}"
        );
    }
    for density in [1, 10, 100] {
        let small = measurements
            .iter()
            .find(|m| m.0 == 100 && m.1 == density)
            .unwrap();
        let large = measurements
            .iter()
            .find(|m| m.0 == 1000 && m.1 == density)
            .unwrap();
        assert!(
            large.3 <= small.3 + 128,
            "tenfold history growth increased foreground work: {small:?} to {large:?}"
        );
    }
}

#[tokio::test]
async fn same_cut_interpretation_continuations_advance_view_without_claiming_new_admissions() {
    let fixture = Fixture::new().await;
    let prior = fixture.store.checkpoint().await.unwrap();
    let baseline = fixture.store.validate_for_publication().await.unwrap();
    let snapshot = fixture
        .store
        .apply(
            prior.fence_token,
            &prior.snapshot,
            &ProjectionBatch {
                through_cut: 0,
                continuation: true,
                mapper_states: BTreeMap::from([("continuation".into(), serde_json::json!(1))]),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .snapshot;
    assert_eq!(snapshot.evidence_cut, 0);
    assert_eq!(snapshot.view_revision, 1);
    assert!(
        fixture
            .store
            .apply(
                prior.fence_token,
                &snapshot,
                &ProjectionBatch {
                    through_cut: 0,
                    continuation: true,
                    processed_occurrences: vec![1],
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    let next = fixture
        .store
        .apply(
            prior.fence_token,
            &snapshot,
            &ProjectionBatch {
                through_cut: 0,
                continuation: true,
                mapper_states: BTreeMap::from([("continuation".into(), serde_json::json!(2))]),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .snapshot;
    assert_eq!(next.view_revision, 2);
    let fence = fixture.store.fence(prior.fence_token).await.unwrap();
    fixture
        .store
        .validate_tail(&baseline, &fence, 2)
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_same_cut_snapshot_cannot_overwrite_committed_projection_state() {
    let fixture = Fixture::new().await;
    let stale = fixture.store.checkpoint().await.unwrap();
    let entry = fixture.entry("continued");
    let committed = fixture
        .publish(ProjectionBatch {
            through_cut: stale.snapshot.evidence_cut,
            continuation: true,
            objects: vec![ObjectUpdate::put(entry.clone(), "committed")],
            mapper_states: BTreeMap::from([(
                "continuation".into(),
                serde_json::json!("committed"),
            )]),
            ..ProjectionBatch::default()
        })
        .await;
    let rejected = fixture
        .store
        .apply(
            stale.fence_token,
            &stale.snapshot,
            &ProjectionBatch {
                through_cut: stale.snapshot.evidence_cut,
                continuation: true,
                objects: vec![ObjectUpdate::put(entry.clone(), "stale overwrite")],
                mapper_states: BTreeMap::from([(
                    "continuation".into(),
                    serde_json::json!("stale overwrite"),
                )]),
                ..ProjectionBatch::default()
            },
        )
        .await;
    assert!(
        matches!(rejected, Err(GenerationError::StaleSnapshot { expected, actual }) if expected == stale.snapshot && actual == committed)
    );
    assert_eq!(
        fixture.store.checkpoint().await.unwrap().snapshot,
        committed
    );
    assert_eq!(
        fixture.store.mapper_states(&committed).await.unwrap()["continuation"],
        serde_json::json!("committed")
    );
    assert_eq!(
        fixture
            .store
            .summary(&committed, &entry.object_ref())
            .await
            .unwrap()
            .preview,
        "committed"
    );
}

#[tokio::test]
async fn concurrent_writers_from_one_snapshot_commit_exactly_one_revision() {
    let fixture = Fixture::new().await;
    let second = GenerationStore::open(
        fixture.store.path(),
        fixture.store.manifest().clone(),
        fixture.store.limits().clone(),
    )
    .await
    .unwrap();
    let expected = fixture.store.checkpoint().await.unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let spawn = |store: GenerationStore, name: &'static str| {
        let barrier = barrier.clone();
        let expected = expected.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .apply(
                    expected.fence_token,
                    &expected.snapshot,
                    &ProjectionBatch {
                        through_cut: expected.snapshot.evidence_cut,
                        continuation: true,
                        mapper_states: BTreeMap::from([("winner".into(), serde_json::json!(name))]),
                        ..ProjectionBatch::default()
                    },
                )
                .await
        })
    };
    let first = spawn(fixture.store.clone(), "first");
    let second = spawn(second, "second");
    barrier.wait().await;
    let outcomes = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
        1,
        "both writers accepted the same snapshot: {outcomes:?}"
    );
    let winner = outcomes
        .iter()
        .find_map(|outcome| outcome.as_ref().ok())
        .unwrap();
    assert!(outcomes.iter().any(|outcome| matches!(outcome, Err(GenerationError::StaleSnapshot { expected: stale, actual }) if stale == &expected.snapshot && actual == &winner.snapshot)));
    assert_eq!(
        fixture.store.checkpoint().await.unwrap().snapshot,
        winner.snapshot
    );
    assert_eq!(winner.snapshot.evidence_cut, expected.snapshot.evidence_cut);
    assert_eq!(
        winner.snapshot.view_revision,
        expected.snapshot.view_revision + 1
    );
}

async fn assert_snapshot_mismatch_rejected(change: impl FnOnce(&mut Snapshot)) {
    let fixture = Fixture::new().await;
    let current = fixture.store.checkpoint().await.unwrap();
    let mut mismatch = current.snapshot.clone();
    change(&mut mismatch);
    let rejected = fixture
        .store
        .apply(
            current.fence_token,
            &mismatch,
            &ProjectionBatch {
                through_cut: current.snapshot.evidence_cut,
                continuation: true,
                mapper_states: BTreeMap::from([("invalid".into(), serde_json::json!(true))]),
                ..ProjectionBatch::default()
            },
        )
        .await;
    assert!(
        matches!(rejected, Err(GenerationError::StaleSnapshot { expected, actual }) if expected == mismatch && actual == current.snapshot)
    );
    assert!(matches!(fixture.store.mapper_states(&mismatch).await,
        Err(GenerationError::StaleSnapshot { expected, actual }) if expected == mismatch && actual == current.snapshot));
    assert_eq!(fixture.store.checkpoint().await.unwrap(), current);
    assert!(
        fixture
            .store
            .mapper_states(&current.snapshot)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn another_generation_snapshot_cannot_write_or_restore_mapper_state() {
    assert_snapshot_mismatch_rejected(|snapshot| snapshot.generation = "another-generation".into())
        .await;
}

#[tokio::test]
async fn a_mismatched_evidence_cut_cannot_write_or_restore_mapper_state() {
    assert_snapshot_mismatch_rejected(|snapshot| snapshot.evidence_cut += 1).await;
}

#[tokio::test]
async fn a_mismatched_view_revision_cannot_write_or_restore_mapper_state() {
    assert_snapshot_mismatch_rejected(|snapshot| snapshot.view_revision += 1).await;
}

#[tokio::test]
async fn mapper_restore_rejects_a_snapshot_before_a_same_cut_continuation() {
    let fixture = Fixture::new().await;
    let old = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            mapper_states: BTreeMap::from([("source".into(), serde_json::json!("old"))]),
            ..ProjectionBatch::default()
        })
        .await;
    let current = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            continuation: true,
            mapper_states: BTreeMap::from([("source".into(), serde_json::json!("current"))]),
            ..ProjectionBatch::default()
        })
        .await;
    assert!(matches!(fixture.store.mapper_states(&old).await,
        Err(GenerationError::StaleSnapshot { expected, actual }) if expected == old && actual == current));
    assert_eq!(
        fixture.store.mapper_states(&current).await.unwrap()["source"],
        serde_json::json!("current")
    );
}

#[tokio::test]
async fn mapper_restore_reads_the_checkpoint_and_values_from_one_transaction() {
    let fixture = Fixture::new().await;
    let original = BTreeMap::from([
        ("source".into(), serde_json::json!("original")),
        ("unchanged".into(), serde_json::json!(true)),
    ]);
    let expected = fixture
        .publish(ProjectionBatch {
            through_cut: 1,
            mapper_states: original.clone(),
            ..ProjectionBatch::default()
        })
        .await;
    let second = GenerationStore::open(
        fixture.store.path(),
        fixture.store.manifest().clone(),
        fixture.store.limits().clone(),
    )
    .await
    .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let mut restore = {
        let store = fixture.store.clone();
        let expected = expected.clone();
        let entered = entered.clone();
        let resume = resume.clone();
        tokio::spawn(async move {
            store
                .mapper_states_inner(&expected, Some((&entered, &resume)))
                .await
        })
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        entered.notified().await;
        let current = second
            .apply(
                1,
                &expected,
                &ProjectionBatch {
                    through_cut: expected.evidence_cut,
                    continuation: true,
                    mapper_states: BTreeMap::from([
                        ("source".into(), serde_json::json!("later")),
                        ("new-only".into(), serde_json::json!(true)),
                    ]),
                    ..ProjectionBatch::default()
                },
            )
            .await
            .unwrap()
            .snapshot;
        resume.notify_one();
        let observed = (&mut restore).await.unwrap().unwrap();
        assert_eq!(observed, original);
        assert_eq!(
            second.mapper_states(&current).await.unwrap()["source"],
            serde_json::json!("later")
        );
    })
    .await;
    if result.is_err() {
        restore.abort();
    }
    result.expect("a concurrent WAL writer must commit while mapper restore retains its snapshot");
}
