use super::*;
use crate::trace::generation::PayloadAnchor;
use crate::trace::reading_store::read_scope;
use serde_json::json;

async fn frame(fixture: &Fixture, seq: u64, value: serde_json::Value) {
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    fixture
        .admit(
            ObservationRef {
                issuer: issuer.clone(),
                producer_id: "live-reader".into(),
                producer_epoch: "one".into(),
                producer_seq: seq,
            },
            SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: "live-reader".into(),
                },
                generation: 0,
                locator: SourceLocator::Frame { ordinal: seq },
            },
            &serde_json::to_vec(&value).unwrap(),
            TransportKind::LiveStdout,
        )
        .await;
}
async fn window(store: &GenerationStore, snapshot: &Snapshot) -> (ReadingUnit, PayloadAccess) {
    let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:streamed").unwrap());
    let page = store
        .read_page(
            snapshot,
            &read_scope(&agent, None, "history"),
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
    let ReadingContent::Message { parts, .. } = &unit.content else {
        panic!("live answer must be readable before finalization")
    };
    assert_eq!(parts.len(), 1);
    (unit.clone(), parts[0].payload.clone())
}

#[tokio::test]
async fn live_append_payloads_remain_snapshot_readable_across_replacement_final_and_restart() {
    let fixture = Fixture::new().await;
    let (store, _) = fixture.generation("streaming").await;
    let worker = fixture.worker();
    frame(
        &fixture,
        1,
        json!({"method":"thread/started","params":{"thread":{"id":"streamed"}}}),
    )
    .await;
    frame(&fixture,2,json!({"method":"item/agentMessage/delta","params":{"threadId":"streamed","turnId":"turn","itemId":"answer","delta":"A中"}})).await;
    frame(&fixture,3,json!({"method":"item/agentMessage/delta","params":{"threadId":"streamed","turnId":"turn","itemId":"answer","delta":"A中"}})).await;
    worker
        .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    let first = store.checkpoint().await.unwrap().snapshot;
    let (before, body) = window(&store, &first).await;
    assert_eq!(
        fixture
            .evidence
            .payload_bytes(&body.payload_ref, 100)
            .await
            .unwrap(),
        "A中A中".as_bytes()
    );
    let manifest = fixture
        .evidence
        .payload_manifest(&body.payload_ref)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manifest.chunks.len(), 2);
    assert_eq!(manifest.chunks[0].digest, manifest.chunks[1].digest);
    drop(worker);
    let worker = fixture.worker();
    frame(&fixture,4,json!({"method":"item/started","params":{"threadId":"streamed","turnId":"turn","item":{"type":"agentMessage","id":"answer","text":"B"}}})).await;
    frame(&fixture,5,json!({"method":"item/agentMessage/delta","params":{"threadId":"streamed","turnId":"turn","itemId":"answer","delta":"C"}})).await;
    worker
        .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    let second = store.checkpoint().await.unwrap().snapshot;
    let (replaced, replacement) = window(&store, &second).await;
    assert_eq!(before.address, replaced.address);
    assert_eq!(
        fixture
            .evidence
            .payload_bytes(&replacement.payload_ref, 100)
            .await
            .unwrap(),
        b"BC"
    );
    frame(&fixture,6,json!({"method":"item/completed","params":{"threadId":"streamed","turnId":"turn","item":{"type":"agentMessage","id":"answer","text":"Final differs","phase":"final_answer"}}})).await;
    frame(&fixture,7,json!({"method":"item/agentMessage/delta","params":{"threadId":"streamed","turnId":"turn","itemId":"answer","delta":"late"}})).await;
    worker
        .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    let final_snapshot = store.checkpoint().await.unwrap().snapshot;
    let (finished, final_body) = window(&store, &final_snapshot).await;
    assert_eq!(before.address, finished.address);
    assert_eq!(
        fixture
            .evidence
            .payload_bytes(&final_body.payload_ref, 100)
            .await
            .unwrap(),
        b"Final differs"
    );
    let (_, old_body) = window(&store, &first).await;
    assert_eq!(old_body.payload_ref, body.payload_ref);
    store
        .authorize_payload(
            &first,
            &PayloadAnchor {
                via_ref: body.via_ref.object(),
                containing_agent: body.containing_agent.agent(),
                selection: body.selection.clone(),
                payload_ref: body.payload_ref.clone(),
                body_revision: body.body_revision.clone(),
                representation: body.representation,
                ranges: vec![],
            },
            0,
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .evidence
            .payload_bytes(&body.payload_ref, 100)
            .await
            .unwrap(),
        "A中A中".as_bytes()
    );
}
