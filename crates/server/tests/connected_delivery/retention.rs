use super::*;
use sylph_protocol::{
    trace::{
        AttachmentAvailability, Capture, Fact, ObservationRef, PayloadRef, ResultRepresentation,
        TraceObject, TransportKind,
    },
    SourceAttachmentEvent,
};

#[tokio::test]
async fn native_reread_retains_a_superseded_fragment_before_reporting_completion() {
    let backend = ConnectedBackend::start(false).await;
    let session = backend.create_session("closed native tail seed").await;
    let native = SessionId::decode(&session).unwrap();
    let path = backend.root.path().join("native-tail.jsonl");
    let header = lexical(json!({"type":"session_meta","payload":{"id":native.native_id}}));
    std::fs::write(&path, [header.as_slice(), b"{broken"].concat()).unwrap();
    backend
        .source
        .native_files
        .lock()
        .unwrap()
        .insert(native.native_id.to_string(), path.clone());
    let reread = format!("/api/v1/sessions/{session}/reread");
    let (status, body) = backend.request("POST", &reread, None).await;
    assert_eq!(status, 200, "{body}");
    std::fs::write(&path, &header).unwrap();
    let (status, body) = backend.request("POST", &reread, None).await;
    assert_eq!(status, 200, "{body}");
    let evidence = backend.state.trace.evidence();
    let page = evidence.admissions_after(0, 64).await.unwrap();
    assert!(!page.has_more);
    let terminal = page
        .captures
        .iter()
        .find(|entry| {
            entry.capture.capture_diagnostics.iter().any(|diagnostic| {
                diagnostic.code == sylph_protocol::trace::CaptureDiagnosticCode::IncompleteRecord
            })
        })
        .expect("finite reread completion must include the retired source fragment");
    assert_eq!(terminal.capture.source_position.generation, 0);
    assert_eq!(
        terminal.capture.source_position.locator,
        sylph_protocol::trace::SourceLocator::Jsonl {
            byte_start: header.len() as u64,
            byte_end: header.len() as u64 + 7,
            record_ordinal: 1,
        }
    );
    assert_eq!(
        evidence
            .capture_payload(&terminal.capture.observation_ref, 100)
            .await
            .unwrap(),
        Some(b"{broken".to_vec())
    );
    assert!(matches!(
        evidence
            .receipt(&terminal.capture.observation_ref)
            .await
            .unwrap()
            .unwrap()
            .disposition,
        ReceiptDisposition::Retained { .. }
    ));
    backend.state.trace.rebuild().await.unwrap();
    assert_eq!(
        evidence
            .capture_payload(&terminal.capture.observation_ref, 100)
            .await
            .unwrap(),
        Some(b"{broken".to_vec())
    );
    backend.drained().await;
}

async fn attachment(
    backend: &ConnectedBackend,
    parent: &ObservationRef,
    availability: AttachmentAvailability,
) -> (Capture, SourceAttachmentEvent) {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut after = 0;
        loop {
            let page = backend
                .state
                .trace
                .evidence()
                .admissions_after(after, 64)
                .await
                .unwrap();
            for entry in page.captures {
                if entry.capture.transport_kind != TransportKind::SourceAttachment {
                    continue;
                }
                let bytes = backend
                    .state
                    .trace
                    .evidence()
                    .capture_payload(&entry.capture.observation_ref, 64 * 1024)
                    .await
                    .unwrap()
                    .unwrap();
                let event: SourceAttachmentEvent = serde_json::from_slice(&bytes).unwrap();
                if event.parent_observation == *parent
                    && event.attachment.availability == availability
                {
                    return (entry.capture, event);
                }
            }
            after = page.next_after;
            if !page.has_more {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    })
    .await
    .expect("the actual envagent must report the sidecar transition")
}

async fn assert_runtime_result(backend: &ConnectedBackend, payload: &PayloadRef) {
    backend.state.trace.project_pending().await.unwrap();
    let generation = backend.state.trace.active().await.unwrap();
    let snapshot = generation.checkpoint().await.unwrap().snapshot;
    let mut after = None;
    loop {
        let page = generation
            .retained_claims(
                &snapshot,
                after.as_ref(),
                generation.limits().max_candidates,
                generation.limits().max_batch_bytes,
            )
            .await
            .unwrap();
        if page.claims.iter().any(|claim| matches!(&claim.fact,
            Fact::Declare { object: TraceObject::Result(result) }
                if result.representation == ResultRepresentation::Runtime && result.payload.as_ref() == Some(payload)
        )) {
            return;
        }
        assert!(
            page.has_more,
            "the retained runtime result must still name the captured sidecar bytes"
        );
        after = page.next;
    }
}

#[tokio::test]
async fn vendor_sidecar_removal_preserves_exact_retained_bytes_and_rebuildable_runtime_results() {
    let backend = ConnectedBackend::start(false).await;
    let session = backend.create_session("sidecar retention seed").await;
    let native = SessionId::decode(&session).unwrap();
    let sidecar = backend.root.path().join("runtime-result.json");
    // Lexical whitespace, duplicate unknown keys, and the full output belong to
    // the archive even though the vendor mapper needs only a subset of fields.
    let raw = b" {\"call_id\":\"exec-retained\",\"turn_id\":\"runtime-turn\",\"process_id\":\"7\",\"source\":\"unified_exec_startup\",\"command\":[\"cat\"],\"cwd\":\"/workspace\",\"parsed_cmd\":[],\"exit_code\":0,\"aggregated_output\":\"retained output\\r\\n\\u0000\",\"status\":\"completed\",\"future\":1,\"future\":2} \r\n";
    std::fs::write(&sidecar, raw).unwrap();
    let frame = |payload: Value| {
        lexical(json!({
            "schema_version":1,"rollout_id":"retention-runtime",
            "thread_id":native.native_id,"codex_turn_id":"runtime-turn","payload":payload,
        }))
    };
    backend
        .source
        .emit(&native, frame(json!({"type":"rollout_started"})))
        .await;
    backend.source.emit(&native, frame(json!({
        "type":"tool_call_started","tool_call_id":"exec-retained","kind":{"type":"exec_command"},
    }))).await;
    let parent = backend.source.emit(&native, frame(json!({
        "type":"tool_call_runtime_ended","tool_call_id":"exec-retained","status":"completed",
        "runtime_payload":{"raw_payload_id":"retained-output","path":sidecar,"kind":{"type":"tool_runtime_event"}},
    }))).await;
    let (captured, event) = attachment(&backend, &parent, AttachmentAvailability::Captured).await;
    let payload = event.attachment.payload_ref.unwrap();
    assert_eq!(
        backend
            .state
            .trace
            .evidence()
            .payload_bytes(&payload, raw.len())
            .await
            .unwrap(),
        raw
    );
    assert_runtime_result(&backend, &payload).await;

    // Observe the vendor removing its file only after the complete payload has
    // crossed the spool/socket/ACK path and is readable in server evidence.
    backend.drained().await;
    std::fs::remove_file(&sidecar).unwrap();
    let (_, missing) = attachment(&backend, &parent, AttachmentAvailability::Missing).await;
    assert!(missing.attachment.payload_ref.is_none());
    assert!(backend
        .state
        .trace
        .evidence()
        .receipt(&captured.observation_ref)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        backend
            .state
            .trace
            .evidence()
            .payload_bytes(&payload, raw.len())
            .await
            .unwrap(),
        raw
    );
    assert_runtime_result(&backend, &payload).await;
    backend.state.trace.rebuild().await.unwrap();
    assert!(!sidecar.exists());
    assert_eq!(
        backend
            .state
            .trace
            .evidence()
            .payload_bytes(&payload, raw.len())
            .await
            .unwrap(),
        raw
    );
    assert_runtime_result(&backend, &payload).await;
}
