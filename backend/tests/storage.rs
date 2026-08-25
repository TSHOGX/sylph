//! Consolidation and replay, end to end.
//!
//! The properties here are the ones `docs/storage.md` exists to guarantee: a turn
//! moves out of the journal losslessly, a client can replay exactly what it saw,
//! and history is durable before the journal forgets.

mod common;

use common::TestBackend;
use sylph_web_backend::models::Event;
use sylph_web_backend::storage::cas::BlobStore;
use sylph_web_backend::storage::history::{self, HistoryStore};
use tempfile::TempDir;

async fn history_store() -> (HistoryStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let blobs = BlobStore::new(dir.path().join("cas"));
    let store = HistoryStore::open(dir.path().join("history.db").to_str().unwrap(), blobs)
        .await
        .unwrap();
    (store, dir)
}

fn event(seq: i32, kind: &str, phase: Option<&str>, stream: Option<&str>, text: &str, at_ms: i64) -> Event {
    Event {
        id: None,
        session_id: "s1".to_string(),
        turn_id: Some("turn-1".to_string()),
        seq,
        kind: kind.to_string(),
        phase: phase.map(str::to_string),
        stream_id: stream.map(str::to_string),
        span: None,
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
        created_at_ms: at_ms,
        reconciled_at_ms: None,
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
        event(6, "assistant", Some("completed"), Some("a1"), "I'll read the config.", 1200),
    ];

    let blocks = history::consolidate(&events);
    history.put_blocks(&blocks).await.unwrap();

    let stored = history.blocks("s1", -1).await.unwrap();
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
        let mut e = event(4, "assistant", Some("completed"), Some("b1"), "found it", 1150);
        e.span = Some("s1".to_string());
        e
    };

    history
        .put_blocks(&history::consolidate(&[opened, renamed, closed, child]))
        .await
        .unwrap();

    let stored = history.blocks("s1", -1).await.unwrap();
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
    events.push(event(201, "assistant", Some("completed"), Some("a1"), &full, 2000));

    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    assert_eq!(
        history.block_count("s1").await.unwrap(),
        1,
        "200 deltas and their completion are one block"
    );
    let block = &history.blocks("s1", -1).await.unwrap()[0];
    assert_eq!(HistoryStore::replay_block(block).len(), 200);
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

    let block = &history.blocks("s1", -1).await.unwrap()[0];
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
    let events = vec![event(0, "tool_result", Some("completed"), Some("c1"), &big, 1000)];

    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    let block = &history.blocks("s1", -1).await.unwrap()[0];
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
    for block in history.blocks("s1", -1).await.unwrap() {
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

    assert_eq!(history.block_count("s1").await.unwrap(), 2);
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

    let blocks = history.blocks("s1", -1).await.unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "I was saying");
    assert_eq!(blocks[0].phase.as_deref(), Some("incomplete"));
}

#[tokio::test]
async fn multibyte_deltas_survive_the_round_trip() {
    let (history, _dir) = history_store().await;
    let events = vec![
        event(0, "assistant", Some("delta"), Some("a1"), "我会先", 1000),
        event(1, "assistant", Some("delta"), Some("a1"), "读取配置。", 1050),
        event(2, "assistant", Some("completed"), Some("a1"), "我会先读取配置。", 1100),
    ];

    history
        .put_blocks(&history::consolidate(&events))
        .await
        .unwrap();

    let block = &history.blocks("s1", -1).await.unwrap()[0];
    let replayed = HistoryStore::replay_block(block);
    let rebuilt: String = replayed.iter().map(|d| d.text.as_str()).collect();
    assert_eq!(rebuilt, "我会先读取配置。");
}

fn count_files(root: &std::path::Path) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
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
    assert!(stream.contains("\"phase\":\"delta\""), "no deltas replayed: {stream}");
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

    let before = backend.state.history.block_count(&session_id).await.unwrap();
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
        backend.state.history.block_count(&session_id).await.unwrap(),
        before,
        "every block stays"
    );

    // The harness has no TraceDB, which is a supported degraded state. What matters
    // is that it is reported as one fact about the pass rather than as a per-session
    // fault, and that no watermark claims a check that never happened.
    assert!(
        !report.archive_available,
        "the harness has no archive, and the pass must say so: {report:?}"
    );
    assert_eq!(
        report.failed, 0,
        "an absent archive is a configuration fact, not N session failures: {report:?}"
    );
    assert!(
        backend
            .state
            .history
            .watermark(&session_id)
            .await
            .unwrap()
            .is_none(),
        "'cannot ask' must not be recorded as 'asked, and nothing is safe yet'"
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

    let report = sylph_web_backend::storage::retention::run_retention(
        &history,
        &sylph_web_backend::storage::TerminalAuthority,
        "s1",
    )
    .await
    .unwrap();

    assert_eq!(report.pruned, 0);
    assert_eq!(
        history.watermark("s1").await.unwrap(),
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

    assert!(
        !report.archive_available,
        "the premise of this test is a degraded archive: {report:?}"
    );
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
    let after_first = backend.state.history.block_count(&session_id).await.unwrap();

    backend.state.retention.pass().await;

    assert_eq!(
        backend.state.history.block_count(&session_id).await.unwrap(),
        after_first,
        "a repeated pass must be a no-op, not a slow erosion of history"
    );
}
