//! Process-boundary integration coverage for `sylph.env.v1`.
//!
//! The bridge unit tests use an in-memory duplex.  This test instead launches
//! `envagent-wire-peer` and connects the real bridge state machine to the
//! child's stdin/stdout.  The child speaks only the framed protocol; it has no
//! knowledge of backend internals.  This gives us an OS-process boundary while
//! keeping the test independent of TCP/socket permissions.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::TestBackend;
use sylph_protocol::{EnvironmentId, NativeId, ProducerId, SessionId};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

#[tokio::test]
async fn process_peer_covers_prepare_run_activate_replay_and_rejection() {
    let backend = TestBackend::start().await;
    let bridge = backend.state.env_bridge.clone();
    let offer = bridge
        .create_pairing()
        .await
        .expect("pairing offer should be created");
    let grant = bridge
        .claim_pairing(&offer.code, "wire fixture", "development", Vec::new())
        .await
        .expect("pairing should be accepted");

    let producer = ProducerId::parse("wire-process-producer").unwrap();
    let session_id = SessionId::new(
        grant.environment_id.clone(),
        sylph_protocol::AgentName::parse("fixture").unwrap(),
        NativeId::new("wire-process-session"),
    );
    let turn_id = "wire-process-turn";
    let config = PeerConfig {
        environment: grant.environment_id,
        token: grant.credential,
        installation: grant.installation_id,
        producer,
        workdir: backend.workdir().display().to_string(),
        session_id,
        turn_id: turn_id.to_string(),
    };

    // The child stays alive while the parent drives the real bridge APIs.  Its
    // command assertions make ordering part of the test contract.
    let first = spawn_peer("promote_drop", &config, &bridge).await;
    wait_for_connected(&bridge, &config.environment).await;
    let (draft_id, contained_workdir) = bridge
        .prepare_remote(
            &config.environment,
            "fixture".into(),
            config.workdir.clone(),
            Some("wire fixture".into()),
            false,
            BTreeMap::new().into_iter().collect(),
            None,
            None,
        )
        .await
        .expect("prepare should round-trip through the child process");
    assert_eq!(contained_workdir, config.workdir);

    let draft = bridge
        .remote_draft(&draft_id)
        .await
        .expect("the bridge should retain the prepared draft");
    let capabilities = bridge
        .remote_draft_capabilities(&draft)
        .await
        .expect("draft capabilities should round-trip");
    assert_eq!(capabilities["supports"][0], "queue");

    let (resolved_session, resolved_workdir) = bridge
        .run_remote_initial(
            &draft,
            turn_id,
            sylph_protocol::WireTurnInput {
                text: "wire first turn".into(),
                attachments: Vec::new(),
                command: None,
                args: BTreeMap::new(),
                config: BTreeMap::new(),
            },
        )
        .await
        .expect("run should return the child-assigned native identity");
    assert_eq!(resolved_session, config.session_id);
    assert_eq!(resolved_workdir, config.workdir);

    let now = sylph_server::models::now_ms();
    backend
        .state
        .store
        .create_session_and_turn(
            &sylph_server::models::Session {
                id: resolved_session.clone(),
                workdir: resolved_workdir,
                title: Some("wire fixture".into()),
                inject_identity: false,
                fidelity: sylph_protocol::Fidelity::Stream,
                flags_json: "{}".into(),
                status: "running".into(),
                created_at_ms: now,
                updated_at_ms: now,
            },
            &sylph_server::models::Turn {
                id: turn_id.into(),
                session_id: resolved_session.clone(),
                status: "running".into(),
                origin: "user".into(),
                request_text: "wire first turn".into(),
                command_invocation_id: None,
                idempotency_key: None,
                error_code: None,
                error_message: None,
                created_at_ms: now,
                started_at_ms: Some(now),
                completed_at_ms: None,
                stopped_at_ms: None,
            },
        )
        .await
        .expect("promoted session and turn should be durable");
    bridge
        .activate_remote(&resolved_session)
        .await
        .expect("activation should release the child event stream");

    wait_for_completed(&backend, turn_id).await;
    let events = backend
        .state
        .store
        .turn_events_after(turn_id, -1)
        .await
        .expect("terminal event should be readable");
    let blocks = backend
        .state
        .history
        .turn_blocks(turn_id)
        .await
        .expect("consolidated terminal event should be readable");
    let terminal_count = events
        .iter()
        .filter(|event| event.kind == "turn" && event.text == "wire process complete")
        .count()
        + blocks
            .iter()
            .filter(|block| block.kind == "turn" && block.text == "wire process complete")
            .count();
    assert_eq!(terminal_count, 1);
    finish_peer(first, "promotion peer").await;

    // The first peer deliberately closed after writing the event and before
    // reading the ACK. A fresh process now replays producer sequence 1 after
    // reconnect. The durable receipt must acknowledge it without adding a
    // second canonical event.
    finish_peer(spawn_peer("replay", &config, &bridge).await, "replay peer").await;
    let replayed = backend
        .state
        .store
        .turn_events_after(turn_id, -1)
        .await
        .expect("replayed event should remain readable");
    let replayed_blocks = backend
        .state
        .history
        .turn_blocks(turn_id)
        .await
        .expect("replayed event should remain consolidated");
    let replayed_terminal_count = replayed
        .iter()
        .filter(|event| event.kind == "turn" && event.text == "wire process complete")
        .count()
        + replayed_blocks
            .iter()
            .filter(|block| block.kind == "turn" && block.text == "wire process complete")
            .count();
    assert_eq!(replayed_terminal_count, 1);
    assert_eq!(
        backend
            .state
            .store
            .env_receipt(
                &config.environment,
                &config.producer,
                sylph_protocol::ProducerSeq::new(1),
            )
            .await
            .expect("producer receipt should be readable"),
        Some(0),
        "one producer item creates one canonical event"
    );

    // The new lease is epoch 3.  The child intentionally sends epoch 1, so
    // the bridge must close it before durable ingest.
    finish_peer(
        spawn_peer("stale", &config, &bridge).await,
        "stale epoch peer",
    )
    .await;
    // A known frame with an invalid payload must close the stream rather than
    // being interpreted as an empty event batch.
    finish_peer(
        spawn_peer("malformed", &config, &bridge).await,
        "malformed frame peer",
    )
    .await;
}

struct PeerConfig {
    environment: EnvironmentId,
    token: String,
    installation: String,
    producer: ProducerId,
    workdir: String,
    session_id: SessionId,
    turn_id: String,
}

struct RunningPeer {
    child: Child,
    bridge_task: JoinHandle<anyhow::Result<()>>,
}

async fn spawn_peer(
    mode: &str,
    config: &PeerConfig,
    bridge: &sylph_server::env::EnvBridge,
) -> RunningPeer {
    let mut child = Command::new(env!("CARGO_BIN_EXE_envagent-wire-peer"))
        .arg(mode)
        .env("SYLPH_WIRE_PEER_ENVIRONMENT", config.environment.as_str())
        .env("SYLPH_WIRE_PEER_TOKEN", &config.token)
        .env("SYLPH_WIRE_PEER_INSTALLATION", &config.installation)
        .env("SYLPH_WIRE_PEER_PRODUCER", config.producer.as_str())
        .env("SYLPH_WIRE_PEER_WORKDIR", &config.workdir)
        .env("SYLPH_WIRE_PEER_SESSION", config.session_id.encode())
        .env("SYLPH_WIRE_PEER_TURN", &config.turn_id)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("wire peer should start");
    let reader = child.stdout.take().expect("wire peer stdout is piped");
    let writer = child.stdin.take().expect("wire peer stdin is piped");
    let bridge_task = {
        let bridge = bridge.clone();
        tokio::spawn(async move { bridge.serve_io(reader, writer).await })
    };
    RunningPeer { child, bridge_task }
}

async fn finish_peer(mut peer: RunningPeer, label: &str) {
    let status = tokio::time::timeout(Duration::from_secs(10), peer.child.wait())
        .await
        .expect("wire peer timed out")
        .expect("wire peer wait failed");
    assert!(status.success(), "{label} exited with {status}");
    // A normal child exit closes its stdout, so the bridge's reader returns
    // an EOF error after all durable work has been handled.  Malformed and
    // stale peers close at that same boundary after the bridge rejects their
    // frame.  In both cases the child-side assertions are the signal; this
    // task must still be joined to avoid leaking a writer into later tests.
    let _ = peer.bridge_task.await;
}

async fn wait_for_completed(backend: &TestBackend, turn_id: &str) {
    for _ in 0..100 {
        if backend
            .state
            .store
            .get_turn(turn_id)
            .await
            .expect("turn lookup should succeed")
            .is_some_and(|turn| turn.status == "completed")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("remote turn {turn_id} did not settle");
}

async fn wait_for_connected(
    bridge: &sylph_server::env::EnvBridge,
    environment: &EnvironmentId,
) {
    for _ in 0..100 {
        if bridge.environment_connected(environment).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("wire peer did not establish a lease");
}
