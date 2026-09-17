//! Delivery acceptance uses the production TCP, spool, admission and projection paths.
//! The only synthetic boundary is vendor acquisition; no vendor CLI or tokens are used.
mod common;
#[path = "common/connected.rs"]
mod connected;
use connected::{CaptureAgents, ConnectedBackend};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use sylph_protocol::{SessionId, trace::AgentRef};
use sylph_server::trace::evidence::ReceiptDisposition;

fn lexical(value: Value) -> Vec<u8> {
    format!(" \t{} \r\n", value).into_bytes()
}
fn agent(id: &str) -> String {
    AgentRef::Session(SessionId::decode(id).unwrap()).encode()
}
async fn bootstrap(backend: &ConnectedBackend, session: &str) -> Value {
    let (status, body) = backend
        .request(
            "GET",
            &format!("/api/v1/trace/agents/{}", agent(session)),
            None,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    body
}
async fn visible(backend: &ConnectedBackend, session: &str, needle: &str, answers: usize) -> Value {
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let body = bootstrap(backend, session).await;
            if body["window"]["items"].to_string().contains(needle)
                && body["window"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|row| row["content"]["author"]["kind"] == "agent")
                    .count()
                    >= answers
            {
                return body;
            }
            let token = body["view"]["token"].as_str().unwrap();
            let (status, _) = backend
                .request("DELETE", &format!("/api/v1/trace/views/{token}"), None)
                .await;
            assert_eq!(status, 204);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("committed fixture reply must become visible through the production reader")
}

async fn history_markers(backend: &ConnectedBackend, session: &str) -> Vec<String> {
    let path = format!("/api/v1/trace/agents/{}/history", agent(session));
    let (status, mut page) = backend
        .request("GET", &format!("{path}?limit=32"), None)
        .await;
    assert_eq!(status, 200, "{page}");
    let view = page["view"].clone();
    let token = view["token"].as_str().unwrap();
    let mut markers = Vec::new();
    loop {
        assert_eq!(page["view"], view);
        let items = page["items"].as_array().unwrap();
        assert!(items.len() <= 32);
        let mut prefix: Vec<_> = items
            .iter()
            .filter_map(|item| {
                let preview = item["content"]["parts"][0]["preview"].as_str()?;
                preview
                    .starts_with("connected-history-")
                    .then(|| preview.split_whitespace().next().unwrap().to_owned())
            })
            .collect();
        prefix.append(&mut markers);
        markers = prefix;
        if page["hasBefore"] != true {
            break;
        }
        let before = page["beforeCursor"].as_str().unwrap();
        let (status, previous) = backend
            .request(
                "GET",
                &format!("{path}?limit=32&at_snapshot={token}&before={before}"),
                None,
            )
            .await;
        assert_eq!(status, 200, "{previous}");
        page = previous;
    }
    let (status, _) = backend
        .request("DELETE", &format!("/api/v1/trace/views/{token}"), None)
        .await;
    assert_eq!(status, 204);
    markers
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connected_history_replay_preserves_bytes_and_unblocks_live_reads_after_deletion() {
    let backend = ConnectedBackend::start(false).await;
    let large = backend.create_session("history seed").await;
    let small = backend.create_session("small seed").await;
    let deleted = backend.create_session("deleted seed").await;
    visible(&backend, &small, "Fixture answer", 1).await;
    let frozen = bootstrap(&backend, &small).await;
    backend.drained().await;
    std::fs::write(
        backend.root.path().join("responsive.txt"),
        "small file stays available\n",
    )
    .unwrap();

    // Hold an actual post-commit ACK, then fill the same producer behind it. This
    // opens the backlog window deterministically instead of hoping to catch a race.
    backend.wire.hold_next_receipt();
    let session = SessionId::decode(&large).unwrap();
    let meta = lexical(
        json!({"type":"session_meta","payload":{"id":session.native_id,"cwd":backend.root.path()}}),
    );
    let first = backend.source.emit(&session, meta.clone()).await;
    assert_eq!(backend.wire.held_receipt().await, first);
    let mut expected = vec![(first.clone(), meta)];
    let records = 160;
    for index in 0..records {
        let text = format!(
            "connected-history-{index:04} {}",
            "lexical 内容 ".repeat(1024)
        );
        let bytes = lexical(
            json!({"type":"response_item","payload":{"type":"message","id":format!("history-{index}"),"role":"assistant","content":[{"type":"output_text","text":text}]}}),
        );
        let reference = backend.source.emit(&session, bytes.clone()).await;
        expected.push((reference, bytes));
        if index == records / 2 {
            let start = Instant::now();
            backend.send(&small, "small-live-during-backlog").await;
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "small command acceptance stalled behind retained uploads"
            );
        }
    }
    let stale = backend.source.emit(&SessionId::decode(&deleted).unwrap(), lexical(json!({"type":"event_msg","payload":{"type":"agent_message","message":"deleted queued bytes must not return"}}))).await;
    let (status, body) = backend
        .request("DELETE", &format!("/api/v1/sessions/{deleted}"), None)
        .await;
    assert_eq!(status, 204, "{body}");
    assert!(backend.spool_count().await >= records as i64);
    let files_started = Instant::now();
    let (status, file) = tokio::time::timeout(
        Duration::from_secs(3),
        backend.request(
            "GET",
            &format!("/api/v1/sessions/{small}/files/content?path=responsive.txt"),
            None,
        ),
    )
    .await
    .expect("Files must respond while the capture ACK is unavailable");
    assert_eq!(status, 200, "{file}");
    assert_eq!(file["content"], "small file stays available\n");
    let files_ms = files_started.elapsed().as_millis();
    assert!(
        backend.spool_count().await > 0,
        "the Files measurement must overlap the backlog"
    );

    let epoch = backend.epoch().await;
    let replay_started = Instant::now();
    backend.wire.hold_next_receipt();
    backend.wire.disconnect();
    backend.wait_connected_after(epoch).await;
    assert_eq!(backend.wire.held_receipt().await, first);
    // Reconnect replay must not occupy the command receiver while waiting for an
    // ACK. Holding the replayed receipt makes this invariant independent of speed.
    let (status, file) = tokio::time::timeout(
        Duration::from_secs(3),
        backend.request(
            "GET",
            &format!("/api/v1/sessions/{small}/files/content?path=responsive.txt"),
            None,
        ),
    )
    .await
    .expect("Files must stay responsive during blocked reconnect replay");
    assert_eq!(status, 200, "{file}");
    assert_eq!(file["content"], "small file stays available\n");
    assert!(backend.spool_count().await > 0);
    let epoch = backend.epoch().await;
    backend.wire.disconnect();
    backend.wait_connected_after(epoch).await;
    let latest = visible(&backend, &small, "small-live-during-backlog", 2).await;
    assert_eq!(
        latest["window"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["content"]["author"]["kind"] == "agent")
            .count(),
        2,
        "a rendered submitted input alone does not prove the vendor answer was delivered"
    );
    backend.drained().await;
    let replay_ms = replay_started.elapsed().as_millis();
    let evidence = backend.state.trace.evidence();
    let mut previous = 0;
    let mut bytes = 0;
    for (reference, lexical) in &expected {
        let receipt = evidence.receipt(reference).await.unwrap().unwrap();
        let ReceiptDisposition::Retained { admission_seq } = receipt.disposition else {
            panic!("live history was discarded")
        };
        assert!(
            admission_seq > previous,
            "source arrival order changed on replay"
        );
        previous = admission_seq;
        assert_eq!(
            evidence
                .capture_payload(reference, lexical.len())
                .await
                .unwrap()
                .as_ref(),
            Some(lexical),
            "lexical bytes changed at producer sequence {}",
            reference.producer_seq
        );
        bytes += lexical.len();
    }
    assert!(matches!(
        evidence.receipt(&stale).await.unwrap().unwrap().disposition,
        ReceiptDisposition::DiscardedByDeletion { .. }
    ));
    assert!(
        evidence
            .capture_payload(&stale, 1024)
            .await
            .unwrap()
            .is_none()
    );
    assert!(evidence.accepted_through(&stale).await.unwrap() >= stale.producer_seq);
    let token = frozen["view"]["token"].as_str().unwrap();
    let (status, old) = backend
        .request(
            "GET",
            &format!("/api/v1/trace/agents/{}?at_snapshot={token}", agent(&small)),
            None,
        )
        .await;
    assert_eq!(status, 200, "{old}");
    assert_eq!(
        old["window"]["items"], frozen["window"]["items"],
        "old snapshot mixed in later input or replies"
    );
    assert_eq!(
        backend
            .sends()
            .iter()
            .filter(|entry| entry["text"] == "small-live-during-backlog")
            .count(),
        1
    );
    backend.state.trace.project_pending().await.unwrap();
    backend.state.trace.index_pending().await.unwrap();
    let (status, hits) = backend
        .request(
            "GET",
            &format!(
                "/api/v1/trace/search?q=%22connected-history-0159%22&agent={}",
                agent(&large)
            ),
            None,
        )
        .await;
    assert_eq!(status, 200, "{hits}");
    assert!(
        !hits["items"].as_array().unwrap().is_empty(),
        "the end of retained history must reach search"
    );
    let expected_markers: Vec<_> = (0..records)
        .map(|index| format!("connected-history-{index:04}"))
        .collect();
    assert_eq!(
        history_markers(&backend, &large).await,
        expected_markers,
        "bounded history pages must preserve every retained message in source order"
    );
    backend.state.trace.rebuild().await.unwrap();
    assert_eq!(
        history_markers(&backend, &large).await,
        expected_markers,
        "rebuild from socket-admitted evidence changed history membership or ordering"
    );
    eprintln!(
        "connected_delivery records={} lexical_bytes={} files_ms={} replay_and_visible_ms={} connections={} upload_frames={} final_queue={}",
        expected.len(),
        bytes,
        files_ms,
        replay_ms,
        backend.wire.connections.load(Ordering::SeqCst),
        backend.wire.upload_frames.load(Ordering::SeqCst),
        backend.spool_count().await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn process_restart_replays_a_lost_capture_receipt_without_repeating_vendor_dispatch() {
    let mut backend = ConnectedBackend::start(true).await;
    let session = backend.create_session("restart seed").await;
    visible(&backend, &session, "Fixture answer", 1).await;
    backend.drained().await;
    backend.wire.hold_next_receipt();
    let command = backend
        .send(&session, "restart-preserves-single-dispatch")
        .await;
    let held = backend.wire.held_receipt().await;
    let bytes = backend
        .state
        .trace
        .evidence()
        .capture_payload(&held, 1024 * 1024)
        .await
        .unwrap()
        .unwrap();
    let receipt = backend
        .state
        .trace
        .evidence()
        .receipt(&held)
        .await
        .unwrap()
        .unwrap();
    assert!(
        backend.spool_count().await > 0,
        "lost ACK must leave durable pending bytes"
    );
    let queued = backend.pending_observations().await;
    assert!(queued.contains(&held));
    assert!(
        queued
            .iter()
            .any(|reference| reference.producer_seq > held.producer_seq),
        "the restart must also recover bytes not yet admitted by the server"
    );
    let epoch = backend.epoch().await;
    backend.kill_child();
    backend.start_child();
    backend.wait_connected_after(epoch).await;
    backend.drained().await;
    for reference in queued {
        assert!(
            backend
                .state
                .trace
                .evidence()
                .receipt(&reference)
                .await
                .unwrap()
                .is_some(),
            "restart lost queued observation {}",
            reference.producer_seq
        );
    }
    visible(&backend, &session, "restart-preserves-single-dispatch", 2).await;
    assert_eq!(
        backend
            .state
            .trace
            .evidence()
            .capture_payload(&held, 1024 * 1024)
            .await
            .unwrap()
            .unwrap(),
        bytes
    );
    assert_eq!(
        backend
            .state
            .trace
            .evidence()
            .receipt(&held)
            .await
            .unwrap()
            .unwrap()
            .disposition,
        receipt.disposition
    );
    let retry = backend
        .send(&session, "restart-preserves-single-dispatch")
        .await;
    assert_eq!(retry["commandId"], command["commandId"]);
    assert_eq!(
        backend
            .sends()
            .iter()
            .filter(|entry| entry["text"] == "restart-preserves-single-dispatch")
            .count(),
        1,
        "restarting the actual envagent process must not replay an accepted vendor send"
    );
    eprintln!(
        "connected_restart held_sequence={} connections={} vendor_dispatches={} final_queue={}",
        held.producer_seq,
        backend.wire.connections.load(Ordering::SeqCst),
        backend.sends().len(),
        backend.spool_count().await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "subprocess entry point for the connected runtime crash acceptance"]
async fn connected_runtime_child() {
    let settings: Value =
        serde_json::from_str(&std::env::var("SYLPH_CONNECTED_RUNTIME").unwrap()).unwrap();
    let root = std::path::PathBuf::from(settings["root"].as_str().unwrap());
    let hello: sylph_protocol::Hello = serde_json::from_value(settings["hello"].clone()).unwrap();
    let config = sylph_envagent::EnvAgentConfig {
        environment_id: hello.environment_id.as_ref().unwrap().to_string(),
        producer_id: hello.producer_id.to_string(),
        spool: sylph_envagent::SpoolConfig::new(root.join("spool.db"), 128 * 1024 * 1024),
        path_policy: sylph_envagent::PathPolicy::from_roots(vec![root.clone()]).unwrap(),
    };
    let source = CaptureAgents::new(root.join("vendor-sends.jsonl"));
    sylph_envagent::client::run_runtime_reconnecting_with_adapters(
        settings["address"].as_str().unwrap().into(),
        hello,
        settings["credential"].as_str().unwrap().into(),
        config,
        Arc::new(source),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn unquoted_hyphenated_search_is_a_client_error_and_does_not_poison_the_next_query() {
    let backend = common::TestBackend::start().await;
    let session = backend.create_session("codex").await;
    backend.state.trace.project_pending().await.unwrap();
    backend.state.trace.index_pending().await.unwrap();
    for query in ["connected-history-0159", "answer%20-%20unknown"] {
        let response = backend
            .get_authed(&format!(
                "/api/v1/trace/search?q={query}&agent={}",
                agent(&session)
            ))
            .await;
        let status = response.status();
        let body = common::json_body(response).await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"]["code"], "invalid_search_query");
    }
    let response = backend
        .get_authed(&format!(
            "/api/v1/trace/search?q=answer&agent={}",
            agent(&session)
        ))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        common::json_body(response).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
