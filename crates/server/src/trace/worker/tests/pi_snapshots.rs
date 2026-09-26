//! Protocol-derived append windows, plus retained real RPC frames below.
use super::*;
use crate::trace::reading_store::read_scope;
use serde_json::{Value, json};

fn owner() -> AgentRef {
    AgentRef::Session(SessionId::decode("env7fk2:pi:snapshots").unwrap())
}
fn entry(id: &str, parent: Option<&str>, body: &str) -> Value {
    json!({"type":"message","id":id,"parentId":parent,"message":{"role":"user","content":body}})
}
fn request(id: &str, since: Option<&str>) -> Value {
    let mut value = json!({"id":id,"type":"get_entries"});
    if let Some(since) = since {
        value["since"] = since.into();
    }
    value
}
fn response(id: &str, entries: Vec<Value>, leaf: &str) -> Value {
    json!({"id":id,"type":"response","command":"get_entries","success":true,"data":{"entries":entries,"leafId":leaf}})
}
async fn reopened(store: GenerationStore) -> GenerationStore {
    let path = store.path().to_path_buf();
    let manifest = store.manifest().clone();
    let limits = store.limits().clone();
    store.close().await;
    GenerationStore::open(path, manifest, limits).await.unwrap()
}
async fn frame(f: &Fixture, ordinal: u64, outgoing: bool, value: &Value) -> Capture {
    let issuer = Issuer::Environment(owner().environment().clone());
    f.admit(
        ObservationRef {
            issuer: issuer.clone(),
            producer_id: "snapshot-rpc".into(),
            producer_epoch: "one".into(),
            producer_seq: ordinal + 1,
        },
        SourcePosition {
            source: SourceRef {
                issuer,
                instance: "snapshot-rpc".into(),
            },
            generation: 0,
            locator: SourceLocator::Frame { ordinal },
        },
        &serde_json::to_vec(value).unwrap(),
        if outgoing {
            TransportKind::LiveStdin
        } else {
            TransportKind::LiveStdout
        },
    )
    .await
}
async fn selected(
    f: &Fixture,
    store: &GenerationStore,
    snapshot: &crate::trace::generation::Snapshot,
    expected: &[&str],
) -> SelectionRef {
    let header = store
        .read_row(
            snapshot,
            &read_scope(&owner(), None, "header"),
            &owner().encode(),
        )
        .await
        .unwrap();
    let header: AgentHeader = serde_json::from_value(header.body).unwrap();
    let selection = header.history.selection.unwrap();
    let page = store
        .read_page(
            snapshot,
            &read_scope(&owner(), Some(&selection), "history"),
            None,
            None,
            None,
            16,
            false,
        )
        .await
        .unwrap();
    let mut bodies = Vec::new();
    for row in page.rows {
        let unit: ReadingUnit = serde_json::from_value(row.body).unwrap();
        let ReadingContent::Message {
            parts, delivery, ..
        } = unit.content
        else {
            panic!("verified snapshot lost message content");
        };
        assert!(delivery.is_none());
        assert!(unit.activation.is_none());
        for part in parts {
            assert_eq!(
                f.evidence
                    .payload_bytes(&part.payload.payload_ref, 4096)
                    .await
                    .unwrap(),
                part.preview.as_bytes()
            );
            bodies.push(part.preview);
        }
    }
    assert_eq!(bodies, expected);
    selection
}

#[tokio::test]
async fn paired_pi_snapshot_windows_keep_baseline_bytes_through_restart_reread_and_rebuild() {
    let f = Fixture::for_vendor("pi").await;
    let (mut store, _) = f.generation("snapshot-windows").await;
    let values = vec![
        (
            false,
            json!({"type":"response","command":"get_state","success":true,"data":{"sessionId":"snapshots"}}),
        ),
        (true, request("base", None)),
        (
            false,
            response("base", vec![entry("a", None, "baseline bytes")], "a"),
        ),
        (true, request("b", Some("a"))),
        (
            false,
            response("b", vec![entry("b", Some("a"), "branch B")], "b"),
        ),
        (true, request("empty", Some("b"))),
        (false, response("empty", vec![], "b")),
        (true, request("c", Some("b"))),
        (
            false,
            response("c", vec![entry("c", Some("a"), "branch C")], "c"),
        ),
        (true, request("back", Some("c"))),
        (false, response("back", vec![], "b")),
    ];
    let mut retained = Vec::new();
    let mut first = None;
    let mut first_claim = None;
    for (ordinal, (outgoing, value)) in values.iter().enumerate() {
        retained.push(frame(&f, ordinal as u64, *outgoing, value).await);
        let cut = f.evidence.evidence_cut().await.unwrap();
        let progress = f.worker().catch_up(&store, 1, cut).await.unwrap();
        assert_eq!(progress.suspended_sources, 0);
        assert_eq!(progress.pending_mappings, 0);
        if matches!(ordinal, 2 | 4 | 6 | 8 | 10) {
            let snapshot = store.checkpoint().await.unwrap().snapshot;
            let expected = match ordinal {
                2 => vec!["baseline bytes"],
                8 => vec!["baseline bytes", "branch C"],
                _ => vec!["baseline bytes", "branch B"],
            };
            let selection = selected(&f, &store, &snapshot, &expected).await;
            let claims = claims(&store).await;
            let declaration = claims.iter().find(|claim| matches!(&claim.fact, Fact::Declare { object: TraceObject::HistorySelection(value) } if value.selection_ref == selection)).unwrap();
            assert!(
                declaration
                    .evidence
                    .contains(&retained[ordinal - 1].observation_ref),
                "selection omitted its request proof"
            );
            assert!(
                declaration
                    .evidence
                    .contains(&retained[ordinal].observation_ref)
            );
            if ordinal == 2 {
                first = Some(snapshot);
                first_claim = Some(declaration.clone());
            } else {
                assert!(
                    claims.contains(first_claim.as_ref().unwrap()),
                    "baseline declaration changed when another response arrived"
                );
            }
        }
        // Reopen after requests too: correlation and baseline survive durable state.
        store = reopened(store).await;
    }
    selected(&f, &store, first.as_ref().unwrap(), &["baseline bytes"]).await;
    let before = reader_projection(&f, &store).await;
    let old_claims = claims(&store).await;
    for mut capture in retained {
        capture.observation_ref.producer_id = "reread-rpc".into();
        f.evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
    }
    let cut = f.evidence.evidence_cut().await.unwrap();
    f.worker().catch_up(&store, 1, cut).await.unwrap();
    assert_eq!(claims(&store).await, old_claims);
    assert_eq!(reader_projection(&f, &store).await, before);
    let (rebuilt, _) = f.generation("snapshot-rebuilt").await;
    f.worker().catch_up(&rebuilt, 1, cut).await.unwrap();
    assert_eq!(claims(&rebuilt).await, old_claims);
    assert_eq!(reader_projection(&f, &rebuilt).await, before);
    let snapshot = rebuilt.checkpoint().await.unwrap().snapshot;
    selected(&f, &rebuilt, &snapshot, &["baseline bytes", "branch B"]).await;
}
