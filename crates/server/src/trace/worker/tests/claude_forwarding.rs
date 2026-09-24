//! Protocol-derived retention variations complement the recorded delegation corpus.
use super::*;
use crate::trace::reading_store::read_scope;

const CALL: &[u8]=br#"{"type":"assistant","sessionId":"forward-root","uuid":"call","message":{"role":"assistant","content":[{"type":"tool_use","id":"host","name":"Agent","input":{"prompt":"forwarded prompt"}}]}}"#;
const RESULT: &[u8]=br#"{"type":"user","sessionId":"forward-root","uuid":"return","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"host","content":"hand-back"}]},"toolUseResult":{"agentId":"forward-child","status":"completed"}}"#;
fn root() -> AgentRef {
    AgentRef::Session(SessionId::decode("env7fk2:claude:forward-root").unwrap())
}
fn child() -> AgentRef {
    AgentRef::Embedded {
        anchor_session: root().anchor_session().clone(),
        namespace: "claude.agent".into(),
        native_child_id: "forward-child".into(),
    }
}
async fn admit(
    f: &Fixture,
    name: &str,
    seq: u64,
    raw: &[u8],
    live: bool,
    epoch: &IngestionEpoch,
) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    let capture = Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: name.into(),
            producer_epoch: "retained".into(),
            producer_seq: seq,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer: issuer.clone(),
                instance: name.into(),
            },
            generation: 0,
            locator: if live {
                SourceLocator::Frame { ordinal: seq - 1 }
            } else {
                SourceLocator::Jsonl {
                    byte_start: (seq - 1) * 1000,
                    byte_end: (seq - 1) * 1000 + raw.len() as u64,
                    record_ordinal: seq - 1,
                }
            },
        },
        ingestion_authorization_epoch: epoch.clone(),
        declared_subject_refs: vec![],
        captured_at: TimeEvidence {
            domain: "fixture".into(),
            value: seq as i64,
            unit: TimeUnit::Milliseconds,
            precision: Some(1),
            basis: TimeBasis::ServerReceipt,
        },
        source_clock: None,
        transport_kind: if live {
            TransportKind::LiveStdout
        } else {
            TransportKind::NativeJsonl
        },
        vendor_build: Some("protocol-derived".into()),
        schema_fingerprint: None,
        payload_ref: f.stage(raw).await,
        attachments: vec![],
        capture_diagnostics: vec![],
    };
    f.evidence
        .admit_batch(&issuer, std::slice::from_ref(&capture))
        .await
        .unwrap();
    capture
}
async fn certify(f: &Fixture, name: &str) -> (IngestionScope, IngestionEpoch) {
    let scope = IngestionScope::Inventory(InventoryRef {
        environment: EnvironmentId::parse("env7fk2").unwrap(),
        vendor: AgentName::parse("claude").unwrap(),
        instance: name.into(),
    });
    let epoch = f.evidence.authorize_scope(&scope, &[]).await.unwrap();
    admit(f, name, 1, CALL, false, &epoch).await;
    admit(f, name, 2, RESULT, false, &epoch).await;
    (scope, epoch)
}
async fn forward(f: &Fixture, index: u64) -> Capture {
    let raw=serde_json::to_vec(&serde_json::json!({"type":"user","session_id":"forward-root","uuid":format!("forwarded-{index}"),"parent_tool_use_id":"host","message":{"role":"user","content":"exact forwarded prompt"}})).unwrap();
    admit(f, "sdk", index + 1, &raw, true, &f.epoch).await
}
async fn catch_up(f: &Fixture, store: &GenerationStore) {
    let progress = f
        .worker()
        .catch_up(store, 1, f.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    assert_eq!(progress.suspended_sources, 0);
}
fn forwarded(claim: &Claim) -> bool {
    matches!(claim.claim_ref.address(),ObjectAddress::Occurrence{position,..}if position.source.instance=="sdk")
}

#[tokio::test]
async fn deleting_one_forwarding_certificate_preserves_independent_routing_but_deleting_both_leaves_only_raw_evidence(
) {
    let f = Fixture::for_vendor("claude").await;
    let (first, _) = certify(&f, "proof-one").await;
    let (store, _) = f.generation("before-second-certificate").await;
    let captured = forward(&f, 0).await;
    catch_up(&f, &store).await;
    let (second, _) = certify(&f, "proof-two").await;
    catch_up(&f, &store).await;
    for (index, removed) in [None, Some(&first), Some(&second)].into_iter().enumerate() {
        if let Some(scope) = removed {
            f.evidence.delete_scope(scope).await.unwrap();
        }
        let (current, _) = f.generation(&format!("remaining-proof-{index}")).await;
        catch_up(&f, &current).await;
        let values: Vec<_> = claims(&current)
            .await
            .into_iter()
            .filter(forwarded)
            .collect();
        if index < 2 {
            assert!(!values.is_empty());
            for claim in &values {
                let sources: BTreeSet<_> = claim
                    .evidence
                    .iter()
                    .filter(|e| e.producer_id.starts_with("proof-"))
                    .map(|e| e.producer_id.as_str())
                    .collect();
                assert_eq!(
                    sources.len(),
                    1,
                    "routing certificates must be independent alternatives"
                );
                if index == 1 {
                    assert_eq!(sources, BTreeSet::from(["proof-two"]));
                }
            }
            let snapshot = current.checkpoint().await.unwrap().snapshot;
            let page = current
                .read_page(
                    &snapshot,
                    &read_scope(&child(), None, "history"),
                    None,
                    None,
                    None,
                    8,
                    false,
                )
                .await
                .unwrap();
            assert_eq!(page.rows.len(), 1);
            let unit: ReadingUnit = serde_json::from_value(page.rows[0].body.clone()).unwrap();
            let ReadingContent::Message {
                author,
                parts,
                delivery,
                ..
            } = unit.content
            else {
                panic!("forwarded input missing")
            };
            assert!(matches!(author, ReadAuthor::Unknown));
            assert!(delivery.is_some());
            assert_eq!(parts[0].preview, "exact forwarded prompt");
            assert_eq!(
                f.evidence
                    .payload_bytes(&parts[0].payload.payload_ref, 4096)
                    .await
                    .unwrap(),
                b"exact forwarded prompt"
            );
        } else {
            assert!(
                values.is_empty(),
                "a deleted route retained child-owned facts"
            );
        }
        assert_eq!(f.evidence.payload_bytes(&captured.payload_ref,4096).await.unwrap(),br#"{"message":{"content":"exact forwarded prompt","role":"user"},"parent_tool_use_id":"host","session_id":"forward-root","type":"user","uuid":"forwarded-0"}"#);
    }
}

#[tokio::test]
async fn late_forwarding_proof_continuation_recovers_after_commit_and_matches_a_cold_rebuild() {
    let f = Fixture::for_vendor("claude").await;
    certify(&f, "proof-one").await;
    let count = mapping::claude::forwarding::PAGE_WORK / 2;
    for index in 0..count {
        forward(&f, index as u64).await;
    }
    let (store, _) = f.generation("forwarding-continuation").await;
    catch_up(&f, &store).await;
    let scope = IngestionScope::Inventory(InventoryRef {
        environment: EnvironmentId::parse("env7fk2").unwrap(),
        vendor: AgentName::parse("claude").unwrap(),
        instance: "proof-two".into(),
    });
    let epoch = f.evidence.authorize_scope(&scope, &[]).await.unwrap();
    admit(&f, "proof-two", 1, CALL, false, &epoch).await;
    catch_up(&f, &store).await;
    let before = f.evidence.evidence_cut().await.unwrap();
    admit(&f, "proof-two", 2, RESULT, false, &epoch).await;
    let worker = f.worker();
    let checkpoint = store.checkpoint().await.unwrap();
    let (state, _) = worker
        .load_state(&store, &checkpoint.snapshot)
        .await
        .unwrap();
    let captures = f
        .evidence
        .admissions_after(before, 1)
        .await
        .unwrap()
        .captures;
    let unit = worker
        .prepare_unit(&store, &state, &captures, before + 1)
        .await
        .unwrap();
    assert!(unit.next.pending.values().any(|pending| pending
        .requests
        .contains(&ArchiveRequest::ContinueForwarding)));
    assert!(
        unit.batch.claims.iter().filter(|c| forwarded(c)).count()
            <= mapping::claude::forwarding::PAGE_WORK
    );
    worker.commit_effects(&store, &unit).await.unwrap();
    crate::trace::worker::reconciliation::apply(&store, 1, &checkpoint.snapshot, &unit.batch)
        .await
        .unwrap();
    drop(worker);
    let path = store.path().to_owned();
    let manifest = store.manifest().clone();
    let limits = store.limits().clone();
    store.close().await;
    let restored = GenerationStore::open(path, manifest, limits).await.unwrap();
    catch_up(&f, &restored).await;
    let actual = claims(&restored).await;
    let (cold, _) = f.generation("forwarding-cold").await;
    catch_up(&f, &cold).await;
    assert_eq!(actual, claims(&cold).await);
    assert_eq!(
        actual
            .iter()
            .filter(|c| forwarded(c) && c.rule_id == "claude/record-message")
            .count(),
        count * 2
    );
}

#[tokio::test]
async fn deferred_forwarding_templates_cannot_bypass_the_semantic_expansion_limit() {
    let f = Fixture::for_vendor("claude").await;
    certify(&f, "route-proof").await;
    let (store, _) = f.generation("forwarded-expansion").await;
    catch_up(&f, &store).await;
    let limits = WorkerLimits {
        max_claims_per_observation: 128,
        ..Default::default()
    };
    let content:Vec<_>=(0..limits.max_claims_per_observation).map(|index|serde_json::json!({"type":"tool_use","id":format!("tool-{index}"),"name":"Read","input":{}})).collect();
    let raw=serde_json::to_vec(&serde_json::json!({"type":"assistant","session_id":"forward-root","parent_tool_use_id":"host","uuid":"many-tools","message":{"role":"assistant","content":content}})).unwrap();
    let captured = admit(&f, "sdk", 1, &raw, true, &f.epoch).await;
    let progress = f
        .worker_with(limits)
        .catch_up(&store, 1, f.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        progress.suspended_sources, 1,
        "deferred claims escaped the configured expansion limit"
    );
    assert!(!claims(&store).await.iter().any(forwarded));
    assert_eq!(
        f.evidence
            .payload_bytes(&captured.payload_ref, raw.len())
            .await
            .unwrap(),
        raw
    );
}
