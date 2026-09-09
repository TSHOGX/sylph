//! Consolidation and replay, end to end.
//!
//! The properties here are the ones `docs/storage.md` exists to guarantee: a turn
//! moves out of the journal losslessly, a client can replay exactly what it saw,
//! and history is durable before the journal forgets.

mod common;

use common::TestBackend;
use sylph_protocol::{NativeBlob, NativeRecord};
use sylph_server::api::sessions::block_to_event;
use sylph_server::events::StreamEvent;
use sylph_server::models::Event;
use sylph_server::storage::cas::BlobStore;
use sylph_server::storage::history::{self, HistoryStore, NativeBlobState};
use tempfile::TempDir;

async fn history_store() -> (HistoryStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let blobs = BlobStore::new(dir.path().join("cas"));
    let store = HistoryStore::open(dir.path().join("history.db").to_str().unwrap(), blobs)
        .await
        .unwrap();
    (store, dir)
}

#[tokio::test]
async fn a_native_record_round_trips_unknown_fields_and_sidecar_bytes() {
    let (history, _dir) = history_store().await;
    let record = NativeRecord {
        source: "root.jsonl".to_string(),
        version: 7,
        raw_json: None,
        payload: serde_json::json!({
            "type": "future_record",
            "ignoredByProjection": {"answer": 42}
        }),
        blobs: vec![NativeBlob {
            name: "/tmp/full-output.txt".to_string(),
            size: 13,
            bytes: Some(b"full sidecar".to_vec()),
            locator: None,
            error: None,
        }],
    };
    let id = session_id().encode();
    history.put_native_record(&id, &record).await.unwrap();
    assert_eq!(
        history.native_record(&id, "root.jsonl", 7).await.unwrap(),
        Some(record.payload)
    );
    assert_eq!(
        history
            .native_blob(&id, "root.jsonl", 7, "/tmp/full-output.txt")
            .await
            .unwrap(),
        NativeBlobState::Available(b"full sidecar".to_vec())
    );
}

#[tokio::test]
async fn native_record_bytes_keep_the_vendor_lexical_payload() {
    let (history, _dir) = history_store().await;
    let raw = br#"{ "b": 2, "a": "\\u0061", "b": 3 }"#.to_vec();
    let record = NativeRecord {
        source: "root.jsonl".to_string(),
        version: 8,
        raw_json: Some(raw.clone()),
        payload: serde_json::from_slice(&raw).unwrap(),
        blobs: Vec::new(),
    };
    let id = session_id().encode();
    history.put_native_record(&id, &record).await.unwrap();
    assert_eq!(
        history
            .native_record_bytes(&id, "root.jsonl", 8)
            .await
            .unwrap(),
        Some(raw)
    );
}

#[tokio::test]
async fn native_record_revision_collision_is_reported() {
    let (history, _dir) = history_store().await;
    let id = session_id().encode();
    let first = NativeRecord {
        source: "manifest.json".to_string(),
        version: 123,
        raw_json: Some(br#"{"state":"old"}"#.to_vec()),
        payload: serde_json::json!({"state": "old"}),
        blobs: Vec::new(),
    };
    let second = NativeRecord {
        source: "manifest.json".to_string(),
        version: 123,
        raw_json: Some(br#"{"state":"new"}"#.to_vec()),
        payload: serde_json::json!({"state": "new"}),
        blobs: Vec::new(),
    };
    history.put_native_record(&id, &first).await.unwrap();
    let error = history.put_native_record(&id, &second).await.unwrap_err();
    assert!(error.to_string().contains("revision collision"));
    assert_eq!(history.native_record_failures(), 1);
}

#[tokio::test]
async fn raw_native_records_survive_event_retention() {
    let (history, _dir) = history_store().await;
    let record = NativeRecord {
        source: "manifest.json".to_string(),
        version: 1,
        raw_json: None,
        payload: serde_json::json!({"ignored": "must survive"}),
        blobs: vec![NativeBlob {
            name: "missing-output".to_string(),
            size: 99,
            bytes: None,
            locator: None,
            error: Some("permission denied".to_string()),
        }],
    };
    let id = session_id().encode();
    history.put_native_record(&id, &record).await.unwrap();
    // Event retention operates on folded blocks and CAS references only. A raw source
    // record has its own retention class and must not disappear when the event watermark
    // advances (or when the blob sweep runs).
    history.prune_below(&id, i32::MAX).await.unwrap();
    history.sweep_unreferenced_blobs().await.unwrap();
    assert_eq!(
        history
            .native_record(&id, "manifest.json", 1)
            .await
            .unwrap(),
        Some(serde_json::json!({"ignored": "must survive"}))
    );
}

#[tokio::test]
async fn native_only_blob_survives_the_cas_sweep() {
    let (history, _dir) = history_store().await;
    let record = NativeRecord {
        source: "root.jsonl".to_string(),
        version: 9,
        raw_json: None,
        payload: serde_json::json!({"type": "tool_result"}),
        blobs: vec![NativeBlob {
            name: "output.txt".to_string(),
            size: 12,
            bytes: Some(b"native-only".to_vec()),
            locator: None,
            error: None,
        }],
    };
    let id = session_id().encode();
    history.put_native_record(&id, &record).await.unwrap();
    assert_eq!(history.sweep_unreferenced_blobs().await.unwrap(), 0);
    assert_eq!(
        history
            .native_blob(&id, "root.jsonl", 9, "output.txt")
            .await
            .unwrap(),
        NativeBlobState::Available(b"native-only".to_vec())
    );
}

#[tokio::test]
async fn deferred_native_blob_is_read_and_cached_on_demand() {
    let (history, _dir) = history_store().await;
    let sidecar = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(sidecar.path(), b"deferred body").unwrap();
    let record = NativeRecord {
        source: "root.jsonl".to_string(),
        version: 10,
        raw_json: None,
        payload: serde_json::json!({"type": "tool_result"}),
        blobs: vec![NativeBlob {
            name: "output.txt".to_string(),
            size: 13,
            bytes: None,
            locator: Some(sidecar.path().to_string_lossy().to_string()),
            error: None,
        }],
    };
    let id = session_id().encode();
    history.put_native_record(&id, &record).await.unwrap();
    assert!(matches!(
        history.native_blob(&id, "root.jsonl", 10, "output.txt").await.unwrap(),
        NativeBlobState::Available(bytes) if bytes == b"deferred body"
    ));
    assert_eq!(history.sweep_unreferenced_blobs().await.unwrap(), 0);
}

/// The session these fixtures hang off.
///
/// A `SessionId` cannot be built from a bare string — it goes through the protocol's own
/// parsers — which is the property under test everywhere the type appears.
fn session_id() -> sylph_protocol::SessionId {
    sylph_protocol::SessionId::new(
        sylph_protocol::EnvironmentId::parse("env7fk2").unwrap(),
        sylph_protocol::AgentName::parse("claude").unwrap(),
        sylph_protocol::NativeId::new("s1"),
    )
}

fn event(
    seq: i32,
    kind: &str,
    phase: Option<&str>,
    stream: Option<&str>,
    text: &str,
    at_ms: i64,
) -> Event {
    Event {
        id: None,
        session_id: session_id(),
        turn_id: Some("turn-1".to_string()),
        seq,
        kind: kind.to_string(),
        phase: phase.map(str::to_string),
        stream_id: stream.map(str::to_string),
        span: None,
        interior_state: None,
        name: None,
        text: text.to_string(),
        data_json: "{}".to_string(),
        is_error: false,
        stability: if phase == Some("completed") || phase.is_none() {
            "stable".to_string()
        } else {
            "ephemeral".to_string()
        },
        source: "agent".to_string(),
        arrival_at_ms: at_ms,
        recorded_at_ms: Some(at_ms),
        created_at_ms: at_ms,
    }
}

/// One assistant block that streamed in four deltas, consolidated and replayed.
/// This is the whole point of the design in one test.
#[tokio::test]
async fn a_streamed_block_replays_exactly_what_the_client_saw() {
    let (history, _dir) = history_store().await;

    let events = vec![
        event(0, "user", None, None, "the question", 1000),
        event(1, "assistant", Some("started"), Some("a1"), "", 1100),
        event(2, "assistant", Some("delta"), Some("a1"), "I'll ", 1100),
        event(3, "assistant", Some("delta"), Some("a1"), "read ", 1134),
        event(4, "assistant", Some("delta"), Some("a1"), "the ", 1185),
        event(5, "assistant", Some("delta"), Some("a1"), "config.", 1197),
        event(
            6,
            "assistant",
            Some("completed"),
            Some("a1"),
            "I'll read the config.",
            1200,
        ),
    ];

    let blocks = history::consolidate(&events);
    history.put_blocks(&blocks).await.unwrap();

    let stored = history.blocks(&session_id().encode(), -1).await.unwrap();
    // The user's message and one assistant block: the deltas folded into it.
    assert_eq!(stored.len(), 2);

    let assistant = stored.iter().find(|b| b.kind == "assistant").unwrap();
    assert_eq!(assistant.text, "I'll read the config.");

    let replayed = HistoryStore::replay_block(assistant);
    let texts: Vec<&str> = replayed.iter().map(|d| d.text.as_str()).collect();
    assert_eq!(texts, vec!["I'll ", "read ", "the ", "config."]);
    // Timing is reconstructed too, which is what makes a paced replay possible.
    let arrivals: Vec<i64> = replayed.iter().map(|d| d.at_ms).collect();
    assert_eq!(arrivals, vec![1100, 1134, 1185, 1197]);
}

/// A delegated trajectory keeps its identity through consolidation.
///
/// Found by an E2E failure whose symptom was two layers away: every subagent card
/// in the timeline read "Subagent" after a turn consolidated, which looked like a
/// frontend fallback firing. The name had been dropped here — folding kept only the
/// `completed` event's fields, and a trajectory is named on `started` (or renamed by
/// the `delta` carrying its parsed brief), because its close reports an outcome
/// rather than an identity.
#[tokio::test]
async fn a_trajectory_keeps_the_name_an_earlier_phase_gave_it() {
    let (history, _dir) = history_store().await;

    let mut opened = event(1, "subagent", Some("started"), Some("s1"), "", 1000);
    opened.name = Some("Explore".to_string());
    let mut renamed = event(2, "subagent", Some("delta"), Some("s1"), "", 1100);
    renamed.name = Some("code-reviewer".to_string());
    // Closes with no name of its own, which is what every real adapter does.
    let closed = event(3, "subagent", Some("completed"), Some("s1"), "", 1200);

    let child = {
        let mut e = event(
            4,
            "assistant",
            Some("completed"),
            Some("b1"),
            "found it",
            1150,
        );
        e.span = Some("s1".to_string());
        e
    };

    history
        .put_blocks(&history::consolidate(&[opened, renamed, closed, child]))
        .await
        .unwrap();

    let stored = history.blocks(&session_id().encode(), -1).await.unwrap();
    let trajectory = stored.iter().find(|b| b.kind == "subagent").unwrap();
    assert_eq!(
        trajectory.name.as_deref(),
        Some("code-reviewer"),
        "the latest name any phase gave it, since the brief parses after it opens"
    );

    let nested = stored.iter().find(|b| b.kind == "assistant").unwrap();
    assert_eq!(
        nested.span.as_deref(),
        Some("s1"),
        "without this the subtree flattens onto the root after a reload"
    );
}

/// A trajectory the sweep deliberately left open survives consolidation, still open.
///
/// Both halves are load-bearing. It has to *survive*, because a `subagent` `started`
/// carries no text and so reached neither the completed path nor the leftover-delta
/// path — the whole delegation folded into no block at all, and its children then
/// reattached to the root on the next read. And it has to survive *as open*, because a
/// client reads a trajectory's status off its phase: `started` says running, which is
/// what backgrounded work is, and consolidation is not entitled to invent an outcome
/// nobody reported.
///
/// This became reachable when the turn-settle sweep started skipping backgrounded
/// work. Measured before the fix: a `Workflow`'s backgrounded delegate was absent from
/// the dispatching turn entirely, so the change that stopped a wrong record being
/// written had traded it for no record at all.
#[tokio::test]
async fn a_trajectory_left_open_survives_consolidation_as_running() {
    let (history, _dir) = history_store().await;

    let mut opened = event(1, "subagent", Some("started"), Some("bg-task"), "", 1000);
    opened.name = Some("Background: worker".to_string());
    opened.data_json = r#"{"is_backgrounded":true}"#.to_string();
    let sibling = event(
        2,
        "assistant",
        Some("completed"),
        Some("a1"),
        "dispatched",
        1100,
    );

    history
        .put_blocks(&history::consolidate(&[opened, sibling]))
        .await
        .unwrap();

    let stored = history.blocks(&session_id().encode(), -1).await.unwrap();
    let trajectory = stored
        .iter()
        .find(|b| b.stream_id.as_deref() == Some("bg-task"))
        .expect("the delegation is still in history");

    assert_eq!(
        trajectory.phase.as_deref(),
        Some("started"),
        "still open, because nothing has reported an outcome"
    );
    assert_eq!(trajectory.name.as_deref(), Some("Background: worker"));
    assert!(
        !trajectory.is_error,
        "running work is not an error; only a server-authored close is"
    );
    assert!(
        trajectory
            .data
            .as_ref()
            .and_then(|d| d.get("is_backgrounded"))
            .is_some(),
        "the flag survives, since after this the journal rows are gone"
    );
}

/// A restart closes what history still holds as running.
///
/// The counterpart to the journal's startup sweep, and the gap that preserving open
/// trajectories opens: `started` is true only while the process that would report the
/// outcome is alive. No link survives a restart, so a block left running afterwards is
/// a card that spins forever — the exact failure the sweep exists to prevent,
/// displaced one tier down.
#[tokio::test]
async fn a_restart_closes_a_trajectory_history_still_holds_as_running() {
    let (history, _dir) = history_store().await;

    let mut opened = event(1, "subagent", Some("started"), Some("bg-task"), "", 1000);
    opened.name = Some("Background: worker".to_string());
    history
        .put_blocks(&history::consolidate(&[opened]))
        .await
        .unwrap();

    let closed = history.close_running_trajectories().await.unwrap();
    assert_eq!(closed, 1);

    let stored = history.blocks(&session_id().encode(), -1).await.unwrap();
    let trajectory = stored
        .iter()
        .find(|b| b.stream_id.as_deref() == Some("bg-task"))
        .expect("the delegation");
    assert_eq!(trajectory.phase.as_deref(), Some("completed"));
    assert_eq!(
        trajectory
            .data
            .as_ref()
            .and_then(|d| d.get("closedBy"))
            .and_then(|v| v.as_str()),
        Some("server"),
        "attributed to the server, so a client reports it abandoned rather than failed"
    );

    // Idempotent: a second start finds nothing left running.
    assert_eq!(history.close_running_trajectories().await.unwrap(), 0);
}

/// A late outcome corrects the record in history, not only in the journal.
///
/// Which tier holds a trajectory depends on whether consolidation has run, which no
/// caller can observe — so the correction has to work here too, or a delegation
/// reporting back a second after its turn is fixed while one reporting back a minute
/// later is not.
#[tokio::test]
async fn a_late_outcome_corrects_a_consolidated_trajectory() {
    let (history, _dir) = history_store().await;

    let mut swept = event(1, "subagent", Some("completed"), Some("bg-task"), "", 1000);
    swept.name = Some("Background: worker".to_string());
    swept.data_json = r#"{"closedBy":"server","reason":"abandoned"}"#.to_string();
    swept.is_error = true;
    swept.source = "server".to_string();
    history
        .put_blocks(&history::consolidate(&[swept]))
        .await
        .unwrap();

    let amended = history
        .reconcile_trajectory(
            &session_id().encode(),
            "bg-task",
            "turn-later",
            &sylph_server::supervisor::TrajectoryOutcome {
                text: "worker finished",
                is_error: false,
                reported_on_turn_id: "turn-later",
            },
        )
        .await
        .unwrap();
    assert!(amended);

    let stored = history.blocks(&session_id().encode(), -1).await.unwrap();
    let trajectory = stored
        .iter()
        .find(|b| b.stream_id.as_deref() == Some("bg-task"))
        .expect("the delegation");
    assert_eq!(
        trajectory.text, "worker finished",
        "read back through the same encoding path put_blocks wrote it with"
    );
    assert!(!trajectory.is_error);
    assert!(
        trajectory
            .data
            .as_ref()
            .and_then(|d| d.get("closedBy"))
            .is_none(),
        "the server has stopped claiming this close"
    );
    assert_eq!(
        trajectory.name.as_deref(),
        Some("Background: worker"),
        "the name survives, since only the outcome changed"
    );
}

/// Deltas must not survive as rows. Storing them individually is the ~20× cost
/// the fold exists to avoid.
#[tokio::test]
async fn deltas_become_one_block_rather_than_a_row_each() {
    let (history, _dir) = history_store().await;

    let mut events = vec![event(0, "assistant", Some("started"), Some("a1"), "", 1000)];
    for i in 0..200 {
        events.push(event(
            i + 1,
            "assistant",
            Some("delta"),
            Some("a1"),
            "token ",
            1000 + i as i64 * 5,
        ));
    }
    let full = "token ".repeat(200);
    events.push(event(
        201,
        "assistant",
        Some("completed"),
        Some("a1"),
        &full,
        2000,
    ));

    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    assert_eq!(
        history.block_count(&session_id().encode()).await.unwrap(),
        1,
        "200 deltas and their completion are one block"
    );
    let block = &history.blocks(&session_id().encode(), -1).await.unwrap()[0];
    assert_eq!(HistoryStore::replay_block(block).len(), 200);
}

#[tokio::test]
async fn a_consolidated_block_publishes_when_it_began_and_not_only_when_it_closed() {
    let (history, _dir) = history_store().await;

    // One block spanning a full second: opened at 1000, closed at 2000.
    let events = vec![
        event(0, "assistant", Some("started"), Some("a1"), "", 1000),
        event(1, "assistant", Some("delta"), Some("a1"), "half ", 1500),
        event(
            2,
            "assistant",
            Some("completed"),
            Some("a1"),
            "half done",
            2000,
        ),
    ];
    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    let block = &history.blocks(&session_id().encode(), -1).await.unwrap()[0];
    let wire = block_to_event(block);

    // Consolidation keys the folded row on the *closing* event, so `createdAtMs`
    // alone says only when the block finished. A client computing a duration from
    // durable history then has an end and no beginning — which is why the
    // delegation overview was built believing durations were unreconstructible
    // after a turn settles. The value was computed and stored all along; only the
    // wire dropped it, and an unpublished measurement is indistinguishable from an
    // impossible one.
    assert_eq!(
        wire.started_at_ms,
        Some(1000),
        "a consolidated block reports the arrival of its first phase"
    );
    assert_eq!(
        wire.created_at_ms, 2000,
        "and still reports its close, so the two together bound the work"
    );
}

#[tokio::test]
async fn a_journal_row_reports_no_start_because_it_is_one_arrival() {
    // The inverse of the above, and the reason the field is optional rather than
    // defaulted: a live event happened once, so a start would either duplicate
    // `createdAtMs` or invent a span the event never had. Absent is the honest
    // answer, and it is what lets a client tell streaming from settled.
    let row = Event {
        id: None,
        session_id: session_id(),
        turn_id: Some("t1".to_string()),
        seq: 0,
        kind: "assistant".to_string(),
        phase: Some("delta".to_string()),
        stream_id: Some("a1".to_string()),
        span: None,
        interior_state: None,
        name: None,
        text: "partial".to_string(),
        data_json: "{}".to_string(),
        is_error: false,
        stability: "ephemeral".to_string(),
        source: "runner".to_string(),
        arrival_at_ms: 1000,
        recorded_at_ms: None,
        created_at_ms: 1000,
    };

    assert_eq!(StreamEvent::from_row(&row).started_at_ms, None);
}

#[tokio::test]
async fn an_unstreamed_block_replays_as_a_single_arrival() {
    let (history, _dir) = history_store().await;
    // A tool result arrives complete, with no deltas at all.
    let events = vec![event(
        0,
        "tool_result",
        Some("completed"),
        Some("c1"),
        "file contents",
        1000,
    )];

    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    let block = &history.blocks(&session_id().encode(), -1).await.unwrap()[0];
    assert!(block.folded.is_none());
    let replayed = HistoryStore::replay_block(block);
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].text, "file contents");
}

/// A large payload is externalized, and reads back through the blob store
/// transparently.
#[tokio::test]
async fn a_large_block_is_externalized_and_still_reads_back_whole() {
    let (history, _dir) = history_store().await;
    let big = "x".repeat(20_000);
    let events = vec![event(
        0,
        "tool_result",
        Some("completed"),
        Some("c1"),
        &big,
        1000,
    )];

    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    let block = &history.blocks(&session_id().encode(), -1).await.unwrap()[0];
    assert_eq!(block.text.len(), 20_000);
    assert_eq!(block.text, big);
}

/// The same file read in two turns is one object on disk.
#[tokio::test]
async fn a_payload_repeated_across_turns_is_stored_once() {
    let (history, _dir) = history_store().await;
    let shared = "the same file contents ".repeat(400);

    for seq in [0, 1] {
        let events = vec![event(
            seq,
            "tool_result",
            Some("completed"),
            Some(&format!("c{seq}")),
            &shared,
            1000 + seq as i64,
        )];
        history
            .put_blocks(&history::consolidate(&events))
            .await
            .unwrap();
    }

    let objects = count_files(&_dir.path().join("cas"));
    assert_eq!(objects, 1, "content addressing stores it once");
    // And both blocks still read it.
    for block in history.blocks(&session_id().encode(), -1).await.unwrap() {
        assert_eq!(block.text, shared);
    }
}

/// Consolidation is idempotent, which is what makes a crash between the history
/// insert and the journal delete harmless.
#[tokio::test]
async fn consolidating_the_same_turn_twice_is_harmless() {
    let (history, _dir) = history_store().await;
    let events = vec![
        event(0, "user", None, None, "q", 1000),
        event(1, "assistant", Some("completed"), Some("a1"), "a", 1100),
    ];

    let blocks = history::consolidate(&events);
    history.put_blocks(&blocks).await.unwrap();
    history.put_blocks(&blocks).await.unwrap();

    assert_eq!(
        history.block_count(&session_id().encode()).await.unwrap(),
        2
    );
}

/// The ordering rule: history holds the turn before the journal drops it. A turn
/// consolidated through the real store must be readable from history and gone
/// from the journal, with nothing lost in between.
#[tokio::test]
async fn a_consolidated_turn_leaves_the_journal_and_survives_in_history() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            serde_json::json!({"text": "consolidate me"}),
        )
        .await;
    let turn_id = common::json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&turn_id).await;

    backend.await_consolidation(&turn_id).await;

    // Gone from the journal…
    assert!(
        backend
            .state
            .store
            .turn_events_after(&turn_id, -1)
            .await
            .unwrap()
            .is_empty(),
        "the journal must not keep what history now holds"
    );
    // …and readable from history, with the assistant text intact. The order
    // matters more than either half: history is durable *before* the journal
    // forgets, so there is no instant at which the turn exists in neither.
    let blocks = backend.state.history.turn_blocks(&turn_id).await.unwrap();
    assert!(
        blocks.iter().any(|b| b.text.contains("mock reply")),
        "the assistant reply must survive the move"
    );
}

/// A block whose completion never arrived — the link died mid-answer — is kept as
/// what did arrive. Losing a half-streamed answer is exactly the case a user most
/// wants to see.
#[tokio::test]
async fn a_block_cut_off_mid_stream_keeps_what_arrived() {
    let (history, _dir) = history_store().await;
    let events = vec![
        event(0, "assistant", Some("started"), Some("a1"), "", 1000),
        event(1, "assistant", Some("delta"), Some("a1"), "I was ", 1010),
        event(2, "assistant", Some("delta"), Some("a1"), "saying", 1020),
        // No completed event: the agent died here.
    ];

    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    let blocks = history.blocks(&session_id().encode(), -1).await.unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "I was saying");
    assert_eq!(blocks[0].phase.as_deref(), Some("incomplete"));
}

#[tokio::test]
async fn multibyte_deltas_survive_the_round_trip() {
    let (history, _dir) = history_store().await;
    let events = vec![
        event(0, "assistant", Some("delta"), Some("a1"), "我会先", 1000),
        event(
            1,
            "assistant",
            Some("delta"),
            Some("a1"),
            "读取配置。",
            1050,
        ),
        event(
            2,
            "assistant",
            Some("completed"),
            Some("a1"),
            "我会先读取配置。",
            1100,
        ),
    ];

    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    let block = &history.blocks(&session_id().encode(), -1).await.unwrap()[0];
    let replayed = HistoryStore::replay_block(block);
    let rebuilt: String = replayed.iter().map(|d| d.text.as_str()).collect();
    assert_eq!(rebuilt, "我会先读取配置。");
}

fn count_files(root: &std::path::Path) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                count += 1;
            }
        }
    }
    count
}

/// Consolidation must be invisible to a reader. A turn's transcript reads the
/// same before and after its events move tiers — otherwise history vanishes from
/// the UI moments after a turn finishes, which is the failure the two-tier read
/// path exists to prevent.
#[tokio::test]
async fn a_transcript_reads_the_same_before_and_after_consolidation() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            serde_json::json!({"text": "remember this"}),
        )
        .await;
    let turn_id = common::json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&turn_id).await;

    let before = common::json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/turns"))
            .await,
    )
    .await;

    // Consolidation is scheduled the moment a turn settles, so by now it has
    // usually already run. Await the observable outcome rather than the trigger:
    // asserting on a sweep's return value would be asserting that the automatic
    // path had *not* worked.
    backend.await_consolidation(&turn_id).await;

    let after = common::json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/turns"))
            .await,
    )
    .await;

    assert_eq!(
        before["turns"][0]["events"], after["turns"][0]["events"],
        "the transcript changed when its events moved tiers"
    );
}

/// A client reconnecting to a turn whose events have been consolidated still gets
/// its stream, rather than an empty one.
#[tokio::test]
async fn an_sse_reconnect_after_consolidation_still_replays_the_turn() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            serde_json::json!({"text": "stream me"}),
        )
        .await;
    let turn_id = common::json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&turn_id).await;
    backend.await_consolidation(&turn_id).await;

    let stream = common::body_string(
        backend
            .get_authed(&format!("/api/v1/turns/{turn_id}/events?after_seq=-1"))
            .await,
    )
    .await;

    assert!(
        stream.contains("mock reply"),
        "a consolidated turn must still replay: {stream}"
    );
}

/// Replay emits the deltas a live client saw, not just the settled block.
#[tokio::test]
async fn replay_reproduces_the_deltas_and_not_only_the_final_text() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            serde_json::json!({"text": "replay me"}),
        )
        .await;
    let turn_id = common::json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&turn_id).await;
    backend.await_consolidation(&turn_id).await;

    // speed=0 emits everything immediately, which is what a test wants.
    let stream = common::body_string(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/replay?speed=0"))
            .await,
    )
    .await;

    // The mock streams "mock " then "mock reply", so a faithful replay carries the
    // partial arrival as well as the settled value.
    assert!(
        stream.contains("\"phase\":\"delta\""),
        "no deltas replayed: {stream}"
    );
    assert!(stream.contains("mock reply"));
}

/// Retention has to actually run, and a real session has to survive it.
///
/// This is the test whose absence let the whole subsystem sit unreachable: the
/// authorities were written and unit-tested, and nothing called them. A pass that
/// examines a real consolidated session and leaves its blocks intact is the
/// end-to-end shape — the mock agent has no agent-native store for TraceDB to have
/// ingested, so withholding is the *correct* outcome here, and the assertion is
/// that the pass happened and kept everything rather than that it pruned.
#[tokio::test]
async fn a_retention_pass_examines_a_consolidated_session_and_keeps_unconfirmed_history() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            serde_json::json!({"text": "retain me"}),
        )
        .await;
    let turn_id = common::json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&turn_id).await;
    backend.await_consolidation(&turn_id).await;

    let before = backend
        .state
        .history
        .block_count(&session_id)
        .await
        .unwrap();
    assert!(before > 0, "the session must hold history to retain");

    let report = backend.state.retention.pass().await;

    // The pass reached this session — which is the thing that was missing entirely,
    // and the reason the authorities sat unreachable while fully tested.
    assert_eq!(
        report.examined, 1,
        "the consolidated session must be examined: {report:?}"
    );
    assert_eq!(
        report.blocks_pruned, 0,
        "nothing confirmed it, so nothing may be dropped: {report:?}"
    );
    assert_eq!(
        backend
            .state
            .history
            .block_count(&session_id)
            .await
            .unwrap(),
        before,
        "every block stays"
    );

    // Reachability is reported **per environment**, once per pass rather than per
    // session — the shape that keeps one configuration fact from becoming N faults.
    // The harness's own environment is reachable, so the interesting assertion is the
    // next one: nothing was confirmed, and the pass says *why*.
    assert_eq!(
        report.environments_reachable.len(),
        1,
        "one environment, probed once: {report:?}"
    );
    // **Withheld is read from the authority's own reason, never inferred from an absent
    // bound.** `confirmed_seq.is_none()` cannot separate "answered conservatively" from
    // "never asked", and those call for opposite responses. No native-store bound is
    // derivable until `Adapter::enumerate` lands, so this is `ambiguous` — deliberately
    // not a zero bound, which would read as a completed check.
    assert_eq!(report.withheld, 1, "{report:?}");
    assert_eq!(
        report.withheld_reasons.get("ambiguous"),
        Some(&1),
        "the pass records why it withheld rather than only that it did: {report:?}"
    );
    assert_eq!(
        report.failed, 0,
        "withholding is a defined outcome, not a failure: {report:?}"
    );
    // **A watermark row is written even though nothing was confirmed**, and the
    // direction of this assertion is the opposite of what it used to be — worth stating
    // plainly, because it looks like a weakened test and is not.
    //
    // Before, the harness had no archive at all: the environment could not be asked, so
    // writing a row would have claimed a check that never happened, and the assertion
    // was that no row existed. Here the environment *is* reachable and *did* answer —
    // conservatively, because no native-store bound is derivable yet. That is the other
    // side of the same rule: "checked, and nothing is safe yet" must be distinguishable
    // from "never checked", and an absent row cannot say the first.
    //
    // The case the old assertion guarded still exists and is still guarded: an
    // unreachable environment is skipped entirely by `RetentionScheduler::run_one`,
    // which returns before `run_retention` and therefore writes nothing.
    let watermark = backend
        .state
        .history
        .watermark(&session_id)
        .await
        .unwrap()
        .expect("a pass that examined this session must record that it looked");
    assert_eq!(
        watermark,
        ("envagent".to_string(), None),
        "the environment holding the native store is the authority, and it confirmed \
         no bound"
    );
}

/// With no downstream copy to consult, a pass must still record that it looked.
///
/// This is the counterpart to the unreachable-archive case: a terminal session
/// never asks the archive, so its answer does not depend on one existing, and the
/// watermark row is what distinguishes "correctly keeping everything forever" from
/// "never scheduled". Those two look identical without it, which is how the missing
/// scheduler stayed invisible.
#[tokio::test]
async fn a_terminal_session_records_its_watermark_even_with_no_archive() {
    let (history, _dir) = history_store().await;
    let events = vec![event(
        0,
        "assistant",
        Some("completed"),
        Some("a1"),
        "only copy",
        1000,
    )];
    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    let report = sylph_server::storage::retention::run_retention(
        &history,
        &sylph_server::storage::TerminalAuthority,
        &session_id(),
    )
    .await
    .unwrap();

    assert_eq!(report.pruned, 0);
    assert_eq!(
        history.watermark(&session_id().encode()).await.unwrap(),
        Some(("terminal".to_string(), None)),
        "the row names the authority, so a reader knows why nothing was dropped"
    );
}

/// Bytes are measured on the install where nothing can be pruned.
///
/// This is the load-bearing property, and the one an obvious implementation gets
/// wrong: measuring alongside the watermark write would leave the report empty
/// here, because a delegated session with no reachable archive is deliberately
/// left untouched — no watermark, since "cannot ask" must not be stored as "asked,
/// and nothing is safe". A no-TraceDB install is exactly the one whose history
/// grows without bound, so it is the one that most needs the number.
#[tokio::test]
async fn a_pass_reports_bytes_held_even_with_no_archive_to_confirm_anything() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            serde_json::json!({"text": "measure me"}),
        )
        .await;
    let turn_id = common::json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&turn_id).await;
    backend.await_consolidation(&turn_id).await;

    let report = backend.state.retention.pass().await;

    // The premise: nothing could confirm a bound, so nothing was pruned. Bytes held
    // must still be reported — measuring only where a watermark was written would leave
    // the report empty on exactly the install whose growth is unbounded.
    assert_eq!(report.confirmed, 0, "{report:?}");
    assert_eq!(report.withheld, 1, "{report:?}");
    assert!(
        report.bytes_held > 0,
        "held bytes must be reported without an authority to confirm them: {report:?}"
    );
    assert_eq!(report.sessions_measured, 1);

    // And it names *which* session, which is the whole point: a count of withheld
    // sessions cannot distinguish forty small ones from one enormous one.
    let named = report
        .largest_sessions
        .iter()
        .find(|s| s.session_id == session_id)
        .unwrap_or_else(|| panic!("the session is not named: {report:?}"));
    assert!(named.bytes > 0);
    assert!(named.blocks > 0);
    // One observation is not a trend, so growth is unknown rather than zero.
    assert_eq!(
        named.grew_by, None,
        "a first measurement must not claim the session is stable: {named:?}"
    );

    // `/diagnostics` carries the same numbers, from the same pass, so a reader
    // does not have to reach into the log.
    let body = common::json_body(backend.get_authed("/api/v1/diagnostics").await).await;
    let retention = &body["retention"];
    assert_eq!(retention["bytesHeld"], report.bytes_held);
    assert_eq!(retention["sessionsMeasured"], 1);
    assert_eq!(
        retention["largestSessions"][0]["sessionId"], session_id,
        "the endpoint names the session too: {body}"
    );
    assert!(
        retention["largestSessions"][0].get("grewBy").is_none(),
        "unknown growth is absent rather than zero on the wire: {body}"
    );
}

/// A second turn has to show up as growth, or the report cannot distinguish a
/// session that reached its size and stopped from one still accumulating — which
/// is the distinction that decides whether to go and look at it.
#[tokio::test]
async fn a_growing_session_reports_how_much_it_grew() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let mut sizes = Vec::new();
    for text in ["first turn", "second turn"] {
        let response = backend
            .post_authed(
                &format!("/api/v1/sessions/{session_id}/turns"),
                serde_json::json!({"text": text}),
            )
            .await;
        let turn_id = common::json_body(response).await["turn"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        backend.await_turn(&turn_id).await;
        backend.await_consolidation(&turn_id).await;
        sizes.push(backend.state.retention.pass().await);
    }

    let grown = sizes
        .last()
        .unwrap()
        .largest_sessions
        .iter()
        .find(|s| s.session_id == session_id)
        .expect("the session must be named")
        .clone();

    assert!(
        grown.grew_by.unwrap_or(0) > 0,
        "the second turn must read as growth: {grown:?}"
    );
    assert!(
        grown.grew_over_ms.is_some(),
        "a delta without its interval cannot be read as a rate: {grown:?}"
    );
    assert!(
        sizes.last().unwrap().bytes_grew_by > 0,
        "the pass total must move too: {:?}",
        sizes.last().unwrap()
    );
}

/// Idempotent: passes run every five minutes forever, so a second one over
/// unchanged state must neither drop history nor accumulate rows.
#[tokio::test]
async fn a_second_retention_pass_over_unchanged_history_changes_nothing() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            serde_json::json!({"text": "retain me twice"}),
        )
        .await;
    let turn_id = common::json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&turn_id).await;
    backend.await_consolidation(&turn_id).await;

    backend.state.retention.pass().await;
    let after_first = backend
        .state
        .history
        .block_count(&session_id)
        .await
        .unwrap();

    backend.state.retention.pass().await;

    assert_eq!(
        backend
            .state
            .history
            .block_count(&session_id)
            .await
            .unwrap(),
        after_first,
        "a repeated pass must be a no-op, not a slow erosion of history"
    );
}

// ------------------------------------------------------------------ search
//
// These pin the shape `docs/api/sessions.md` § Search defines, and the decisions that shape
// rests on.

/// A block is findable by its text once consolidated, and the hit resolves to a position.
#[tokio::test]
async fn a_consolidated_block_is_findable_by_its_text() {
    let (history, _dir) = history_store().await;
    history
        .put_blocks(&history::consolidate(&[event(
            0,
            "assistant",
            Some("completed"),
            Some("a1"),
            "the reconnect loop drops every frame",
            1000,
        )]))
        .await
        .unwrap();

    let hits = history.search("reconnect", 10).await.unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, session_id().encode());
    assert_eq!(hits[0].seq, 0);
    assert!(
        hits[0].excerpt.contains("reconnect"),
        "the excerpt is the text containing the query: {}",
        hits[0].excerpt
    );
}

/// A query containing punctuation is a phrase, not an FTS5 expression.
///
/// **Without a hyphen in the query this is invisible, because every single-word query
/// works.** FTS5 reads `-` as NOT, so `needle-one` parses as `needle NOT one` and SQLite
/// answers `no such column: one` — a 500 naming a column, three layers from the text box.
/// Identifiers and flags are exactly what someone searches a transcript for, so this is
/// the common case rather than an edge one.
#[tokio::test]
async fn a_query_with_punctuation_is_searched_literally() {
    let (history, _dir) = history_store().await;

    history
        .put_blocks(&history::consolidate(&[event(
            0,
            "assistant",
            Some("completed"),
            Some("a1"),
            "passing needle-one to the parser",
            1000,
        )]))
        .await
        .unwrap();

    // The hyphenated term itself: an expression parse errors instead of answering.
    let hits = history.search("needle-one", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "a hyphenated query must match as a phrase");

    // A quote is escaped rather than ending the phrase, which would be a syntax error.
    assert!(
        history.search("say \"hello", 10).await.is_ok(),
        "an unbalanced quote must not reach FTS5's parser as syntax"
    );

    // The phrase is literal, so a term the block does not hold finds nothing rather than
    // matching on a fragment of it.
    assert!(
        history.search("needle-two", 10).await.unwrap().is_empty(),
        "a phrase match must not fall back to matching either half"
    );
}

/// **Decoded text is indexed, not the stored bytes**, which is why `blocks_fts` cannot be
/// an external-content table.
///
/// A block over the compression threshold is stored zstd, and one over the externalization
/// threshold is not in the row at all — its bytes live under `blobs/` and `text_inline` is
/// NULL. An external-content index would hold compressed bytes for the first and nothing
/// for the second, so **the rows most worth finding — the large ones — would be exactly
/// the unfindable ones.** That failure is silent: the index builds, queries run, and they
/// simply never match.
#[tokio::test]
async fn a_compressed_and_an_externalized_block_are_both_findable() {
    let (history, _dir) = history_store().await;

    // Over the 512-byte compression threshold, and compressible.
    let compressed = format!("{} needleone", "padding ".repeat(200));
    // Over the 4 KB externalization threshold, so the text leaves the row entirely.
    let externalized = format!("{} needletwo", "filler ".repeat(2000));

    history
        .put_blocks(&history::consolidate(&[
            event(
                0,
                "assistant",
                Some("completed"),
                Some("a1"),
                &compressed,
                1000,
            ),
            event(
                1,
                "tool_result",
                Some("completed"),
                Some("t1"),
                &externalized,
                2000,
            ),
        ]))
        .await
        .unwrap();

    assert_eq!(
        history.search("needleone", 10).await.unwrap().len(),
        1,
        "a zstd-compressed block must still be findable"
    );
    assert_eq!(
        history.search("needletwo", 10).await.unwrap().len(),
        1,
        "an externalized block must still be findable — this is the row an \
         external-content index would silently lose"
    );
}

/// **A result is one session carrying its best match, never one row per block.**
///
/// `limit` bounds sessions for the same reason: grouped client-side after a row limit, a
/// session with thirty hits would consume the whole page and crowd out every other
/// session's single good hit.
#[tokio::test]
async fn many_matches_in_one_session_are_one_result_carrying_a_count() {
    let (history, _dir) = history_store().await;
    let blocks: Vec<_> = (0..5)
        .map(|seq| {
            event(
                seq,
                "assistant",
                Some("completed"),
                Some(&format!("a{seq}")),
                "the widget failed again",
                1000 + seq as i64,
            )
        })
        .collect();
    history
        .put_blocks(&history::consolidate(&blocks))
        .await
        .unwrap();

    let hits = history.search("widget", 10).await.unwrap();

    assert_eq!(
        hits.len(),
        1,
        "five matching blocks in one session are one result"
    );
    assert_eq!(hits[0].hits, 5, "and the count says how many matched");
}

/// Pruning removes index entries with the rows, in one transaction.
///
/// `blocks_fts` is standalone rather than external-content, so nothing removes its entries
/// automatically — and a hit pointing at a pruned block is worse than no hit, because a
/// client follows it and finds nothing.
#[tokio::test]
async fn pruning_a_block_removes_it_from_the_index() {
    let (history, _dir) = history_store().await;
    history
        .put_blocks(&history::consolidate(&[event(
            0,
            "assistant",
            Some("completed"),
            Some("a1"),
            "findable until pruned",
            1000,
        )]))
        .await
        .unwrap();
    assert_eq!(history.search("findable", 10).await.unwrap().len(), 1);

    history
        .prune_below(&session_id().encode(), 0)
        .await
        .unwrap();

    assert!(
        history.search("findable", 10).await.unwrap().is_empty(),
        "a hit that resolves to nothing is worse than no hit"
    );
}

/// Re-consolidating a turn must not index it twice.
///
/// `put_blocks` is idempotent by `INSERT OR REPLACE`, but `blocks_fts` has no primary key
/// to replace on — so the insert is preceded by a delete. Without it a crash between the
/// history write and the journal delete, which consolidation is explicitly designed to
/// survive, would return the same session twice from one query with a doubled count.
#[tokio::test]
async fn re_consolidating_a_turn_does_not_index_it_twice() {
    let (history, _dir) = history_store().await;
    let blocks = history::consolidate(&[event(
        0,
        "assistant",
        Some("completed"),
        Some("a1"),
        "indexed exactly once",
        1000,
    )]);

    history.put_blocks(&blocks).await.unwrap();
    history.put_blocks(&blocks).await.unwrap();

    let hits = history.search("indexed", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].hits, 1,
        "the second pass replaced rather than added"
    );
}

/// Chinese is searchable by word, which is the whole reason the tokenizer is `jieba`.
///
/// **`unicode61` cannot do this**: it splits on non-alphanumerics, so an entire run of Han
/// characters is one token and a query for any substring of it matches nothing. This test
/// fails against the stock tokenizer, which is what makes it evidence about the index
/// rather than about SQLite.
#[tokio::test]
async fn chinese_text_is_searchable_by_word() {
    let (history, _dir) = history_store().await;

    history
        .put_blocks(&history::consolidate(&[event(
            0,
            "assistant",
            Some("completed"),
            Some("a1"),
            "检索引擎的中文分词很重要",
            1000,
        )]))
        .await
        .unwrap();

    // A word *inside* the run, which is the case `unicode61` cannot answer.
    assert_eq!(
        history.search("中文", 10).await.unwrap().len(),
        1,
        "a word inside a run of Han characters must match"
    );
    assert_eq!(history.search("分词", 10).await.unwrap().len(), 1);

    // English in the same index still works, including inflections — one tokenizer serves
    // both languages, so adopting jieba must not cost English stemming.
    history
        .put_blocks(&history::consolidate(&[event(
            1,
            "assistant",
            Some("completed"),
            Some("a2"),
            "the parser drops frames",
            2000,
        )]))
        .await
        .unwrap();
    assert_eq!(
        history.search("drop", 10).await.unwrap().len(),
        1,
        "English stemming must survive the CJK tokenizer"
    );
}

/// **No user input is a malformed query**, because the text is quoted into a phrase rather
/// than parsed as an FTS5 expression.
///
/// This test previously asserted the opposite — that an unbalanced quote *errors* — which
/// pinned the defect as intended behaviour. FTS5 has its own expression syntax, and a
/// user's search string is not written in it: punctuation is ordinary text to the person
/// typing it, so reaching the expression parser at all is the bug. See
/// `a_query_with_punctuation_is_searched_literally`.
#[tokio::test]
async fn no_user_input_reaches_the_fts5_expression_parser() {
    let (history, _dir) = history_store().await;

    for query in [
        "\"unbalanced",
        "needle-one",
        "a AND b",
        "NOT x",
        "*",
        "(",
        "col:v",
    ] {
        assert!(
            history.search(query, 10).await.is_ok(),
            "`{query}` must be searched as text, not parsed as syntax"
        );
    }
}

/// A trajectory keeps the parent that spawned it, not the one that last spoke to it.
///
/// A long-lived delegate is messaged again after it starts, and each message is a fresh host
/// `tool_use`, so its later notifications name a different call than the one that spawned it.
/// Measured on a real store: 29 delegates carry notifications from between 2 and 9 distinct
/// hosts, and the spawning call is the first in 95% of them.
///
/// Folding kept the *latest* `started`/`delta`, which is right for a name or a progress
/// snapshot and wrong for a span: it reparented the block under whichever call last spoke, so
/// a card moved after its turn settled while the live tree — which keeps the first — still
/// showed it where it belonged. One fact with two derivations, disagreeing at opposite ends.
#[tokio::test]
async fn a_trajectory_keeps_the_parent_that_spawned_it() {
    let (history, _dir) = history_store().await;

    let mut opened = event(1, "subagent", Some("started"), Some("s1"), "", 1000);
    opened.span = Some("toolu_spawning_call".to_string());
    opened.name = Some("reviewer".to_string());

    // A later message to the same agent, under a different host call. It legitimately
    // revises the name; it does not re-parent the delegation.
    let mut messaged = event(2, "subagent", Some("delta"), Some("s1"), "", 1100);
    messaged.span = Some("toolu_messaging_call".to_string());
    messaged.name = Some("reviewer (round two)".to_string());

    history
        .put_blocks(&history::consolidate(&[opened, messaged]))
        .await
        .unwrap();

    let stored = history.blocks(&session_id().encode(), -1).await.unwrap();
    let trajectory = stored.iter().find(|b| b.kind == "subagent").unwrap();
    assert_eq!(
        trajectory.span.as_deref(),
        Some("toolu_spawning_call"),
        "a delegation has one parent, however many times it is spoken to afterwards"
    );
    assert_eq!(
        trajectory.name.as_deref(),
        Some("reviewer (round two)"),
        "a later phase still revises the name, which is a snapshot rather than an identity"
    );
}
