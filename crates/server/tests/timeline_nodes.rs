mod common;

use common::TestBackend;
use serde_json::{json, Value};
use sylph_protocol::SessionId;
use sylph_server::models::{Event, Turn};

async fn seed(backend: &TestBackend, count: i32) -> String {
    seed_with_text(backend, count, "bounded preview ".repeat(64)).await
}

async fn seed_with_text(backend: &TestBackend, count: i32, text: String) -> String {
    let id = backend.create_session("mock").await;
    let session_id = SessionId::decode(&id).unwrap();
    let turn: Turn = serde_json::from_value(json!({
        "id": format!("large-{count}"), "sessionId": id, "status": "running",
        "origin": "user", "requestText": "Inspect the workspace", "createdAtMs": 1
    }))
    .unwrap();
    backend.state.store.create_turn(&turn).await.unwrap();
    for start in (0..count).step_by(256) {
        let events: Vec<Event> = (start..(start + 256).min(count))
            .map(|seq| Event {
                id: None,
                session_id: session_id.clone(),
                turn_id: Some(turn.id.clone()),
                seq,
                kind: "thinking".into(),
                phase: Some("completed".into()),
                stream_id: Some(format!("think-{seq}")),
                span: None,
                interior_state: None,
                name: None,
                text: text.clone(),
                data_json: "{}".into(),
                is_error: false,
                stability: "stable".into(),
                source: "agent".into(),
                arrival_at_ms: i64::from(seq),
                recorded_at_ms: None,
                created_at_ms: i64::from(seq),
            })
            .collect();
        backend.state.store.append_events(&events).await.unwrap();
    }
    id
}

#[tokio::test]
async fn oversized_turn_first_screen_is_bounded() {
    let backend = TestBackend::start().await;
    for count in [2_000, 20_000] {
        let id = seed(&backend, count).await;
        let mut samples = Vec::new();
        let mut bytes = 0;
        for _ in 0..7 {
            let start = std::time::Instant::now();
            let page: Value = common::json_body(
                backend
                    .get_authed(&format!("/api/v1/sessions/{id}/nodes"))
                    .await,
            )
            .await;
            samples.push(start.elapsed().as_micros());
            bytes = serde_json::to_vec(&page).unwrap().len();
            assert_eq!(page["nodes"].as_array().unwrap().len(), 2);
            assert!(bytes <= sylph_server::db::timeline::PAGE_BYTES);
        }
        let journal_bytes = std::fs::metadata(&backend.state.config.journal_path)
            .unwrap()
            .len();
        let wal_bytes = std::fs::metadata(format!("{}-wal", backend.state.config.journal_path))
            .map(|m| m.len())
            .unwrap_or(0);
        let work = backend
            .state
            .store
            .node_page(&id, "", None, false, 40)
            .await
            .unwrap()
            .work;
        let cold = samples[0];
        samples.sort();
        let start = std::time::Instant::now();
        let response = backend
            .get_authed(&format!("/api/v1/sessions/{id}/turns"))
            .await;
        let raw = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .unwrap();
        let export: Value = serde_json::from_slice(&raw).unwrap();
        eprintln!(
            "timeline_measurement {}",
            json!({"events":count,"nodes":2,"nodeBytes":bytes,"coldUs":cold,"medianUs":samples[3],"fullTurnBytes":serde_json::to_vec(&export).unwrap().len(),"fullTurnUs":start.elapsed().as_micros(),"journalBytes":journal_bytes,"journalWalBytes":wal_bytes,"nodePoolWaitUs":work.pool_wait_us,"nodeReadUs":work.projection_us})
        );
    }
}

#[tokio::test]
async fn collapsed_and_direct_child_pages_do_not_open_archived_bodies() {
    let backend = TestBackend::start().await;
    let text = "unopened archive body ".repeat(1000);
    let id = seed_with_text(&backend, 20, text.clone()).await;
    sylph_server::storage::history::consolidate_turn(
        &backend.state.store,
        &backend.state.history,
        "large-20",
    )
    .await
    .unwrap();
    let page = backend
        .state
        .store
        .node_page(&id, "g:0", None, false, 40)
        .await
        .unwrap();
    assert_eq!(page.nodes.len(), 20);
    let address = &page.nodes[0].id;
    let bodies = &backend.state.config.blobs_dir;
    let held = bodies.with_extension("held-by-body-read-test");
    // Only disposable fixture bodies are hidden. A body read now fails loudly,
    // making accidental hydration observable even if a page discards the result.
    std::fs::rename(bodies, &held).unwrap();
    for parent in ["", "g:0"] {
        let response = backend
            .get_authed(&format!("/api/v1/sessions/{id}/nodes?parent={parent}"))
            .await;
        assert!(response.status().is_success());
        let page = common::json_body(response).await;
        assert!(!page["nodes"].as_array().unwrap().is_empty());
    }
    let body_path = format!("/api/v1/sessions/{id}/nodes/{address}/body");
    assert!(!backend.get_authed(&body_path).await.status().is_success());
    std::fs::rename(&held, bodies).unwrap();
    let response = backend.get_authed(&body_path).await;
    assert!(response.status().is_success());
    assert_eq!(common::json_body(response).await["text"], text);
    eprintln!(
        "lazy_body_measurement {}",
        json!({"pageReadsWithBodiesUnavailable":2,"children":20,"bodyBytes":text.len(),"unavailableBodyFails":true,"restoredBodyMatches":true})
    );
}

// Explicit protocol axes keep each event fixture legible at its call site.
#[allow(clippy::too_many_arguments)]
async fn append(
    backend: &TestBackend,
    id: &str,
    seq: i32,
    kind: &str,
    phase: &str,
    stream: &str,
    span: Option<&str>,
    failed: bool,
) {
    let event = Event {
        id: None,
        session_id: SessionId::decode(id).unwrap(),
        turn_id: Some("large-0".into()),
        seq,
        kind: kind.into(),
        phase: Some(phase.into()),
        stream_id: Some(stream.into()),
        span: span.map(str::to_owned),
        interior_state: None,
        name: Some(
            if kind == "tool_call" {
                "Read"
            } else {
                "Worker"
            }
            .into(),
        ),
        text: format!("{kind} {stream}"),
        data_json: "{}".into(),
        is_error: failed,
        stability: "stable".into(),
        source: "agent".into(),
        arrival_at_ms: i64::from(seq),
        recorded_at_ms: None,
        created_at_ms: i64::from(seq),
    };
    backend.state.store.append_event(&event).await.unwrap();
}

#[tokio::test]
async fn groups_keep_narrative_order_and_late_results_correct_ancestor_counts() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(
        &backend, &id, 0, "subagent", "started", "worker", None, false,
    )
    .await;
    append(
        &backend,
        &id,
        1,
        "tool_call",
        "completed",
        "read",
        Some("worker"),
        false,
    )
    .await;
    append(
        &backend,
        &id,
        2,
        "assistant",
        "completed",
        "reply",
        None,
        false,
    )
    .await;
    append(
        &backend,
        &id,
        3,
        "thinking",
        "completed",
        "later",
        None,
        false,
    )
    .await;
    let roots = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    assert_eq!(
        roots
            .nodes
            .iter()
            .map(|n| n.event.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["user", "execution", "assistant", "execution"]
    );
    let group = &roots.nodes[1];
    assert_eq!((group.tools, group.running), (1, 1));
    let boundary = format!("{}:", roots.snapshot + 1);
    append(
        &backend,
        &id,
        4,
        "tool_result",
        "completed",
        "read",
        Some("worker"),
        true,
    )
    .await;
    let changes = backend
        .state
        .store
        .node_page(&id, "", Some(&boundary), true, 40)
        .await
        .unwrap();
    assert_eq!(changes.nodes.len(), 1);
    assert_eq!((changes.nodes[0].failed, changes.nodes[0].running), (1, 1));
    append(
        &backend,
        &id,
        5,
        "tool_result",
        "completed",
        "read",
        Some("worker"),
        false,
    )
    .await;
    append(
        &backend,
        &id,
        6,
        "subagent",
        "completed",
        "worker",
        None,
        false,
    )
    .await;
    let roots = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    assert_eq!((roots.nodes[1].failed, roots.nodes[1].running), (0, 0));
    let before = serde_json::to_value(&roots.nodes).unwrap();
    sylph_server::storage::history::consolidate_turn(
        &backend.state.store,
        &backend.state.history,
        "large-0",
    )
    .await
    .unwrap();
    let after = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    assert_eq!(before, serde_json::to_value(&after.nodes).unwrap());
}

#[tokio::test]
async fn child_pages_and_revision_pages_have_no_gaps_or_duplicates() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 110).await;
    let mut cursor = None;
    let mut ids = std::collections::HashSet::new();
    loop {
        let page = backend
            .state
            .store
            .node_page(&id, "g:0", cursor.as_deref(), false, 7)
            .await
            .unwrap();
        assert!(page.nodes.len() <= 7);
        for node in page.nodes {
            assert!(ids.insert(node.id));
        }
        if !page.has_more {
            break;
        }
        cursor = page.next_cursor;
    }
    assert_eq!(ids.len(), 110);
    let mut cursor = Some("0:".to_owned());
    let mut changed = std::collections::HashSet::new();
    loop {
        let page = backend
            .state
            .store
            .node_page(&id, "g:0", cursor.as_deref(), true, 3)
            .await
            .unwrap();
        for node in page.nodes {
            assert!(changed.insert(node.id));
        }
        if !page.has_more {
            break;
        }
        cursor = page.next_cursor;
    }
    assert_eq!(ids, changed);
}

#[tokio::test]
async fn a_parent_declared_after_its_child_moves_without_leaving_a_root_duplicate() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(
        &backend,
        &id,
        0,
        "thinking",
        "completed",
        "thought",
        Some("child"),
        false,
    )
    .await;
    let before = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    assert!(before.nodes.iter().any(|n| n.id == "s:child"));
    append(
        &backend, &id, 1, "subagent", "started", "parent", None, false,
    )
    .await;
    append(
        &backend,
        &id,
        2,
        "subagent",
        "started",
        "child",
        Some("parent"),
        false,
    )
    .await;
    let after = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    assert!(!after.nodes.iter().any(|n| n.id == "s:child"));
    let changes = backend
        .state
        .store
        .node_page(
            &id,
            "",
            Some(&format!("{}:", before.snapshot + 1)),
            true,
            40,
        )
        .await
        .unwrap();
    assert!(changes.nodes.iter().any(|n| n.id == "s:child" && n.removed));
    assert_eq!(after.nodes.iter().map(|n| n.thinking).sum::<i64>(), 1);
}

#[tokio::test]
async fn a_promoted_tool_keeps_its_arguments_and_its_delegates_report() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(
        &backend,
        &id,
        0,
        "tool_call",
        "completed",
        "host",
        None,
        false,
    )
    .await;
    append(
        &backend,
        &id,
        1,
        "thinking",
        "completed",
        "thought",
        Some("host"),
        false,
    )
    .await;
    append(
        &backend,
        &id,
        2,
        "subagent",
        "completed",
        "host",
        None,
        false,
    )
    .await;
    append(
        &backend,
        &id,
        3,
        "tool_result",
        "completed",
        "host",
        None,
        false,
    )
    .await;
    let roots = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    let group = roots
        .nodes
        .iter()
        .find(|n| n.event.kind == "execution")
        .unwrap();
    assert_eq!((group.tools, group.thinking, group.running), (1, 1, 0));
    let children = backend
        .state
        .store
        .node_page(&id, &group.id, None, false, 40)
        .await
        .unwrap();
    let host = &children.nodes[0];
    assert_eq!(host.event.kind, "tool_call");
    assert_eq!(host.event.text, "tool_call host");
    assert_eq!(host.trajectory.as_ref().unwrap().text, "subagent host");
}

async fn measured_children(backend: &TestBackend, id: &str, parent: &str) -> usize {
    use sqlx::Connection;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let mut conn = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&backend.state.config.journal_path),
    )
    .await
    .unwrap();
    // This test-only switch restores the original scan shape against disposable data.
    if std::env::var_os("SYLPH_TIMELINE_TEST_SCAN").is_some() {
        sqlx::query("DROP INDEX IF EXISTS timeline_children")
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let steps = Arc::new(AtomicUsize::new(0));
    let measured = steps.clone();
    conn.lock_handle()
        .await
        .unwrap()
        .set_progress_handler(1, move || {
            measured.fetch_add(1, Ordering::Relaxed);
            true
        });
    let rows = sqlx::query(sylph_server::db::timeline::CHILDREN_SQL)
        .bind(id)
        .bind(parent)
        .bind(i64::MAX)
        .bind(i64::MAX)
        .bind("")
        .bind(sylph_server::db::timeline::PAGE_NODES + 1)
        .fetch_all(&mut conn)
        .await
        .unwrap();
    conn.lock_handle().await.unwrap().remove_progress_handler();
    assert!(rows.len() <= sylph_server::db::timeline::PAGE_NODES as usize + 1);
    steps.load(Ordering::Relaxed)
}

#[tokio::test]
async fn indexed_child_query_work_does_not_grow_with_a_collapsed_turn() {
    let backend = TestBackend::start().await;
    let small = seed(&backend, 2000).await;
    let small_steps = measured_children(&backend, &small, "g:0").await;
    let large = seed(&backend, 20000).await;
    let large_steps = measured_children(&backend, &large, "g:0").await;
    eprintln!("timeline_query_steps small={small_steps} large={large_steps}");
    assert!(
        large_steps <= small_steps * 2,
        "query work grew with collapsed history: {small_steps} -> {large_steps}"
    );
}

#[tokio::test]
async fn a_streaming_projection_patch_does_not_walk_settled_siblings() {
    use sqlx::Connection;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let backend = TestBackend::start().await;
    let mut measurements = Vec::new();
    for count in [2_000, 20_000] {
        let id = seed(&backend, count).await;
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(&backend.state.config.journal_path),
        )
        .await
        .unwrap();
        let mut event: Event =
            sqlx::query_as("SELECT * FROM events WHERE turn_id=? ORDER BY seq DESC LIMIT 1")
                .bind(format!("large-{count}"))
                .fetch_one(&mut conn)
                .await
                .unwrap();
        event.seq = count;
        event.stream_id = Some("measured-live-stream".into());
        event.phase = Some("started".into());
        event.text = "Reading a file".into();
        backend.state.store.append_event(&event).await.unwrap();
        event.seq += 1;
        event.phase = Some("delta".into());
        event.text = " increment".into();
        let steps = Arc::new(AtomicUsize::new(0));
        let measured = steps.clone();
        conn.lock_handle()
            .await
            .unwrap()
            .set_progress_handler(1, move || {
                measured.fetch_add(1, Ordering::Relaxed);
                true
            });
        let started = std::time::Instant::now();
        let mut tx = conn.begin_with("BEGIN IMMEDIATE").await.unwrap();
        // The production projector runs inside the event transaction. Roll back
        // this isolated measurement so it cannot leave an unjournalled address.
        sylph_server::db::timeline::project(&mut tx, &event)
            .await
            .unwrap();
        let elapsed = started.elapsed().as_micros();
        let work = steps.load(Ordering::Relaxed);
        tx.lock_handle().await.unwrap().remove_progress_handler();
        tx.rollback().await.unwrap();
        measurements.push(work);
        eprintln!(
            "timeline_patch_measurement {}",
            json!({"siblings":count,"vmSteps":work,"elapsedUs":elapsed,"session":id})
        );
    }
    assert!(
        measurements[1] <= measurements[0] * 2,
        "a streaming patch scanned settled siblings: {measurements:?}"
    );
}

#[tokio::test]
async fn file_browsing_and_node_reads_finish_inside_a_history_write_lock_window() {
    use sqlx::Connection;
    let backend = TestBackend::start().await;
    let id = seed(&backend, 2000).await;
    std::fs::write(
        backend.workdir().join("responsiveness.txt"),
        "visible while history is locked",
    )
    .unwrap();
    let mut conn = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&backend.state.config.history_path),
    )
    .await
    .unwrap();
    let held = conn.begin_with("BEGIN IMMEDIATE").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let started = std::time::Instant::now();
        let response = backend
            .get_authed(&format!("/api/v1/sessions/{id}/files"))
            .await;
        assert!(response.status().is_success());
        let files = common::json_body(response).await;
        assert!(files.to_string().contains("responsiveness.txt"));
        let file_us = started.elapsed().as_micros();
        let started = std::time::Instant::now();
        let nodes = backend
            .get_authed(&format!("/api/v1/sessions/{id}/nodes"))
            .await;
        assert!(nodes.status().is_success());
        eprintln!(
            "timeline_file_lock_window {}",
            json!({"fileUs":file_us,"nodeUs":started.elapsed().as_micros()})
        );
    })
    .await
    .expect("file and timeline reads must finish before the competing writer releases its lock");
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn files_remain_responsive_during_native_ingestion_archiving_search_and_cleanup() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use sylph_protocol::NativeRecord;
    use sylph_server::storage::history::consolidate_turn;
    let backend = TestBackend::start().await;
    let browsing = seed(&backend, 0).await;
    let archived = seed(&backend, 8_000).await;
    let cleaned = seed(&backend, 4_000).await;
    consolidate_turn(&backend.state.store, &backend.state.history, "large-4000")
        .await
        .unwrap();
    let mut external = backend
        .state
        .history
        .turn_blocks("large-4000")
        .await
        .unwrap()
        .remove(0);
    external.text = "bounded external cleanup body ".repeat(200);
    backend.state.history.put_blocks(&[external]).await.unwrap();
    let environment = backend.environment_id().await;
    let response = backend
        .post_authed(
            &format!("/api/v1/environments/{environment}/imports"),
            json!({
                "mode":"import_and_watch", "agents":["mock"], "workdir":backend.workdir(),
                "idempotencyKey":"mixed-load-subscription"
            }),
        )
        .await;
    assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
    let job = common::json_body(response).await;
    let job_id = job["job"]["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let job = common::json_body(
                backend
                    .get_authed(&format!("/api/v1/imports/{job_id}"))
                    .await,
            )
            .await;
            if job["job"]["status"] == "completed" {
                assert_eq!(job["job"]["failed"], 0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    backend.agents.add_stored_session("discovered-under-load");
    std::fs::write(
        backend.workdir().join("mixed-load.txt"),
        "visible under mixed load",
    )
    .unwrap();
    let barrier = tokio::sync::Barrier::new(6);
    let done: [AtomicBool; 3] = std::array::from_fn(|_| AtomicBool::new(false));
    let searches = AtomicUsize::new(0);
    let started = Instant::now();
    let native = async {
        barrier.wait().await;
        let begin = started.elapsed().as_micros();
        for version in 0..2_000 {
            let raw = format!(
                "{{ \"future\":{},\"payload\":\"{}\" }}",
                version,
                "native bytes ".repeat(350)
            )
            .into_bytes();
            let record = NativeRecord {
                source: "synthetic.jsonl".into(),
                version,
                payload: serde_json::from_slice(&raw).unwrap(),
                raw_json: Some(raw),
                blobs: vec![],
            };
            backend
                .state
                .history
                .put_native_record(&browsing, &record)
                .await
                .unwrap();
        }
        done[0].store(true, Ordering::SeqCst);
        json!({"startUs":begin,"endUs":started.elapsed().as_micros(),"records":2000})
    };
    let archive = async {
        barrier.wait().await;
        let begin = started.elapsed().as_micros();
        let blocks = consolidate_turn(&backend.state.store, &backend.state.history, "large-8000")
            .await
            .unwrap();
        assert_eq!(blocks, 8_000);
        done[1].store(true, Ordering::SeqCst);
        json!({"startUs":begin,"endUs":started.elapsed().as_micros(),"blocks":blocks})
    };
    let cleanup = async {
        barrier.wait().await;
        let begin = started.elapsed().as_micros();
        let pruned = backend
            .state
            .history
            .prune_below(&cleaned, i32::MAX)
            .await
            .unwrap();
        let mut sweep_passes = 0;
        let swept = loop {
            sweep_passes += 1;
            let swept = backend
                .state
                .history
                .sweep_unreferenced_blobs()
                .await
                .unwrap();
            if swept > 0 {
                break swept;
            }
            // Publication intentionally takes priority over GC. A deferred pass
            // is not deletion; retry and report when an actual body was removed.
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(pruned, 4_000);
        assert!(swept > 0, "cleanup must remove a real external body");
        done[2].store(true, Ordering::SeqCst);
        json!({"startUs":begin,"endUs":started.elapsed().as_micros(),"pruned":pruned,"swept":swept,"sweepPasses":sweep_passes})
    };
    let search = async {
        barrier.wait().await;
        while done.iter().any(|flag| !flag.load(Ordering::SeqCst)) {
            let response = backend
                .get_authed("/api/v1/search?q=bounded&limit=20")
                .await;
            assert!(response.status().is_success());
            let _body = common::json_body(response).await;
            searches.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let sync = async {
        barrier.wait().await;
        let begin = started.elapsed().as_micros();
        let refreshed = sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap();
        let id = SessionId::decode(&format!("{environment}:mock:discovered-under-load")).unwrap();
        assert!(backend
            .state
            .store
            .get_session(&id)
            .await
            .unwrap()
            .is_some());
        json!({"startUs":begin,"endUs":started.elapsed().as_micros(),"refreshed":refreshed,"discovered":id.encode()})
    };
    let files = async {
        barrier.wait().await;
        let mut samples = Vec::new();
        while samples.len() < 40 || done.iter().any(|flag| !flag.load(Ordering::SeqCst)) {
            let before = started.elapsed().as_micros();
            let active = done.iter().all(|flag| !flag.load(Ordering::SeqCst));
            let response = tokio::time::timeout(
                Duration::from_secs(2),
                backend.get_authed(&format!("/api/v1/sessions/{browsing}/files")),
            )
            .await
            .expect("file request stalled behind storage work");
            assert!(response.status().is_success());
            let body = common::json_body(response).await;
            assert!(body.to_string().contains("mixed-load.txt"));
            samples.push(json!({"startUs":before,"durationUs":started.elapsed().as_micros()-before,"allWritersActive":active}));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        samples
    };
    let (native, archive, cleanup, (), samples, sync) =
        tokio::time::timeout(Duration::from_secs(60), async {
            tokio::join!(native, archive, cleanup, search, files, sync)
        })
        .await
        .expect("mixed workloads must all make progress");
    assert!(
        samples
            .iter()
            .any(|sample| sample["allWritersActive"] == true),
        "the fixture must open an actual overlapping workload window"
    );
    assert!(searches.load(Ordering::SeqCst) > 0);
    assert_eq!(
        backend
            .state
            .history
            .native_records(&browsing)
            .await
            .unwrap()
            .len(),
        2_000
    );
    let raw = backend
        .state
        .history
        .native_record_bytes(&browsing, "synthetic.jsonl", 1999)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        raw,
        format!(
            "{{ \"future\":1999,\"payload\":\"{}\" }}",
            "native bytes ".repeat(350)
        )
        .into_bytes()
    );
    assert!(backend
        .state
        .store
        .turn_events_after("large-8000", -1)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        backend
            .state
            .history
            .turn_blocks("large-8000")
            .await
            .unwrap()
            .len(),
        8_000
    );
    assert!(backend
        .state
        .history
        .turn_blocks("large-4000")
        .await
        .unwrap()
        .is_empty());
    let hits = backend.state.history.search("bounded", 20).await.unwrap();
    assert!(hits.iter().any(|hit| hit.session_id == archived));
    assert!(!hits.iter().any(|hit| hit.session_id == cleaned));
    eprintln!(
        "mixed_storage_measurement {}",
        json!({"native":native,"archive":archive,"cleanup":cleanup,"sync":sync,
        "searches":searches.load(Ordering::SeqCst),"files":samples,
        "transport":"in-process authenticated router; file operations cross the envagent bridge"})
    );
}

#[tokio::test]
async fn malformed_parent_cycles_remain_reachable_and_report_the_repair() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(
        &backend,
        &id,
        0,
        "subagent",
        "started",
        "a",
        Some("b"),
        false,
    )
    .await;
    append(
        &backend,
        &id,
        1,
        "subagent",
        "started",
        "b",
        Some("a"),
        false,
    )
    .await;
    let roots = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    let repaired = roots.nodes.iter().find(|n| n.id == "s:b").unwrap();
    assert!(repaired
        .structure_error
        .as_ref()
        .unwrap()
        .contains("Cyclic"));
    let children = backend
        .state
        .store
        .node_page(&id, &repaired.id, None, false, 40)
        .await
        .unwrap();
    let group = children
        .nodes
        .iter()
        .find(|n| n.event.kind == "execution")
        .unwrap();
    let grandchildren = backend
        .state
        .store
        .node_page(&id, &group.id, None, false, 40)
        .await
        .unwrap();
    assert!(grandchildren.nodes.iter().any(|n| n.id == "s:a"));
}

#[tokio::test]
async fn reusing_a_block_stream_id_in_a_later_turn_cannot_replace_the_first_reply() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(
        &backend,
        &id,
        0,
        "assistant",
        "completed",
        "reply",
        None,
        false,
    )
    .await;
    let turn:Turn=serde_json::from_value(json!({"id":"second","sessionId":id,"status":"running","origin":"user","requestText":"Second question","createdAtMs":20})).unwrap();
    backend.state.store.create_turn(&turn).await.unwrap();
    let mut event = backend
        .state
        .store
        .session_events(&SessionId::decode(&id).unwrap())
        .await
        .unwrap()
        .remove(0);
    event.seq = 1;
    event.turn_id = Some(turn.id);
    event.text = "The second reply".into();
    event.id = None;
    backend.state.store.append_event(&event).await.unwrap();
    let roots = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    let replies: Vec<_> = roots
        .nodes
        .iter()
        .filter(|n| n.event.kind == "assistant")
        .collect();
    assert_eq!(replies.len(), 2);
    assert_ne!(replies[0].id, replies[1].id);
    assert_eq!(replies[0].event.text, "assistant reply");
    assert_eq!(replies[1].event.text, "The second reply");
}

#[tokio::test]
async fn search_targets_load_only_their_direct_parent_window_across_archiving() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 110).await;
    let before = backend.state.store.node_window(&id, 55).await.unwrap();
    assert_eq!(before.parent, "g:0");
    assert!(before.nodes.len() <= sylph_server::db::timeline::PAGE_NODES as usize);
    assert_eq!(before.nodes.last().unwrap().event.seq, 55);
    assert_eq!(
        before.target_id,
        Some(before.nodes.last().unwrap().id.clone())
    );
    sylph_server::storage::history::consolidate_turn(
        &backend.state.store,
        &backend.state.history,
        "large-110",
    )
    .await
    .unwrap();
    let after = backend.state.store.node_window(&id, 55).await.unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}

#[tokio::test]
async fn input_only_turns_keep_admission_order_without_an_event_between_them() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    let turn:Turn=serde_json::from_value(json!({"id":"aaa-second","sessionId":id,"status":"completed","origin":"user","requestText":"Second input without output","createdAtMs":1})).unwrap();
    backend.state.store.create_turn(&turn).await.unwrap();
    let page = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    assert_eq!(
        page.nodes
            .iter()
            .map(|node| node.event.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Inspect the workspace", "Second input without output"]
    );
    let newest = backend
        .state
        .store
        .node_page(&id, "", None, false, 1)
        .await
        .unwrap();
    assert_eq!(newest.nodes[0].event.text, "Second input without output");
    let older = backend
        .state
        .store
        .node_page(&id, "", newest.next_cursor.as_deref(), false, 1)
        .await
        .unwrap();
    assert_eq!(older.nodes[0].event.text, "Inspect the workspace");
}

#[tokio::test]
async fn the_opening_user_title_is_written_with_the_event_not_during_a_later_read() {
    use sqlx::Connection;
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(&backend, &id, 0, "user", "completed", "prompt", None, false).await;
    let mut conn = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&backend.state.config.journal_path)
            .read_only(true),
    )
    .await
    .unwrap();
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM sessions WHERE native_id=?")
        .bind(SessionId::decode(&id).unwrap().native_id.as_str())
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(title.as_deref(), Some("user prompt"));
}

#[tokio::test]
async fn percent_encoded_session_paths_cannot_bypass_the_observe_grant() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    let registered = backend
        .post(
            "/api/v1/auth/register",
            json!({"name":"observer without a grant"}),
        )
        .await;
    let cookie = registered
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let encoded = id.replace(':', "%3A");
    for suffix in ["turn-state", "nodes/search?q=private", "turns", "nodes"] {
        let response = backend
            .get_with_cookie(&format!("/api/v1/sessions/{encoded}/{suffix}"), &cookie)
            .await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::FORBIDDEN,
            "{suffix} escaped the session authorization gate"
        );
    }
}

#[tokio::test]
async fn an_expanded_stream_keeps_its_prefix_when_its_owner_turn_is_archived() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(
        &backend,
        &id,
        0,
        "subagent",
        "started",
        "background",
        None,
        false,
    )
    .await;
    let mut event = backend
        .state
        .store
        .session_events(&SessionId::decode(&id).unwrap())
        .await
        .unwrap()
        .remove(0);
    event.id = None;
    event.kind = "thinking".into();
    event.stream_id = Some("thought".into());
    event.span = Some("background".into());
    event.name = None;
    event.seq = 1;
    event.text = String::new();
    backend.state.store.append_event(&event).await.unwrap();
    event.seq = 2;
    event.phase = Some("delta".into());
    event.text = "a".repeat(1500);
    backend.state.store.append_event(&event).await.unwrap();
    let old = backend
        .state
        .store
        .consolidation_snapshot("large-0")
        .await
        .unwrap();
    backend
        .state
        .history
        .put_blocks(&sylph_server::storage::history::consolidate(&old.events))
        .await
        .unwrap();
    let later:Turn=serde_json::from_value(json!({"id":"later","sessionId":id,"status":"running","origin":"agent","requestText":"","createdAtMs":4})).unwrap();
    backend.state.store.create_turn(&later).await.unwrap();
    event.seq = 3;
    event.turn_id = Some(later.id);
    event.text = "b".repeat(1000);
    backend.state.store.append_event(&event).await.unwrap();
    let node = backend
        .state
        .store
        .node_window(&id, 3)
        .await
        .unwrap()
        .target_id
        .unwrap();
    for evict in [false, true] {
        if evict {
            backend.state.store.delete_turn_events(&old).await.unwrap();
        }
        let response = backend
            .get_authed(&format!("/api/v1/sessions/{id}/nodes/{node}/body"))
            .await;
        let body = common::json_body(response).await;
        assert_eq!(
            body["text"],
            format!("{}{}", "a".repeat(1500), "b".repeat(1000)),
            "archive eviction={evict} changed the logical body"
        );
    }
}

#[tokio::test]
async fn closing_a_turn_stops_unfinished_root_activity_without_failing_background_work() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(
        &backend,
        &id,
        0,
        "thinking",
        "started",
        "unfinished",
        None,
        false,
    )
    .await;
    append(
        &backend,
        &id,
        1,
        "subagent",
        "started",
        "background",
        None,
        false,
    )
    .await;
    append(
        &backend,
        &id,
        2,
        "thinking",
        "started",
        "child",
        Some("background"),
        false,
    )
    .await;
    backend
        .state
        .store
        .finish_turn_at("large-0", "cancelled", None, None, 10)
        .await
        .unwrap();
    let root = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    let group = root
        .nodes
        .iter()
        .find(|node| node.event.kind == "execution")
        .unwrap();
    assert_eq!(
        group.running, 1,
        "only the independent background scope remains active"
    );
    assert_eq!(
        group.failed, 0,
        "missing completion is not a reported failure"
    );
}

#[tokio::test]
async fn an_error_kind_is_visible_in_collapsed_summaries_without_a_redundant_error_flag() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(
        &backend, &id, 0, "subagent", "started", "worker", None, false,
    )
    .await;
    append(
        &backend,
        &id,
        1,
        "error",
        "completed",
        "fault",
        Some("worker"),
        false,
    )
    .await;
    let page = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    assert_eq!(page.nodes.iter().map(|node| node.failed).sum::<i64>(), 1);
}

#[tokio::test]
async fn archive_publication_preserves_events_that_arrive_after_its_snapshot() {
    use sqlx::Connection;
    let backend = TestBackend::start().await;
    let id = seed_with_text(&backend, 1, "archive snapshot body ".repeat(1000)).await;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&backend.state.config.history_path),
    )
    .await
    .unwrap();
    let held = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let journal = backend.state.store.clone();
    let history = backend.state.history.clone();
    let archive = tokio::spawn(async move {
        sylph_server::storage::history::consolidate_turn(&journal, &history, "large-1")
            .await
            .unwrap()
    });
    fn has_file(path: &std::path::Path) -> bool {
        std::fs::read_dir(path).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|entry| entry.path().is_file() || has_file(&entry.path()))
        })
    }
    // CAS publication precedes the held SQL writer. Keep that window open until
    // the late event commits, so the test cannot pass by scheduling the race away.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !has_file(&backend.state.config.blobs_dir) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let mut event = backend
        .state
        .store
        .turn_events_for_consolidation("large-1")
        .await
        .unwrap()
        .remove(0);
    event.id = None;
    event.seq = 1;
    event.stream_id = Some("late".into());
    event.text = "late retained body ".repeat(1000);
    backend.state.store.append_event(&event).await.unwrap();
    held.rollback().await.unwrap();
    archive.await.unwrap();
    assert_eq!(
        backend
            .state
            .store
            .turn_events_for_consolidation("large-1")
            .await
            .unwrap()
            .len(),
        1
    );
    let transcript = common::json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{id}/turns"))
            .await,
    )
    .await;
    let events = transcript["turns"][0]["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        2,
        "a late same-turn tail must not hide its archived prefix"
    );
    assert!(events[0]["text"]
        .as_str()
        .unwrap()
        .starts_with("archive snapshot body"));
    assert!(events[1]["text"]
        .as_str()
        .unwrap()
        .starts_with("late retained body"));
    let node = backend
        .state
        .store
        .node_window(&id, 1)
        .await
        .unwrap()
        .target_id
        .unwrap();
    let response = backend
        .get_authed(&format!("/api/v1/sessions/{id}/nodes/{node}/body"))
        .await;
    assert!(response.status().is_success());
    assert_eq!(common::json_body(response).await["text"], event.text);
    sylph_server::storage::history::consolidate_turn(
        &backend.state.store,
        &backend.state.history,
        "large-1",
    )
    .await
    .unwrap();
    let response = backend
        .get_authed(&format!("/api/v1/sessions/{id}/nodes/{node}/body"))
        .await;
    assert_eq!(common::json_body(response).await["text"], event.text);
}

#[tokio::test]
async fn accepted_native_input_has_one_prompt_address_in_the_event_transaction() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    backend
        .state
        .store
        .set_turn_request_text(&SessionId::decode(&id).unwrap(), "large-0", "")
        .await
        .unwrap();
    append(
        &backend,
        &id,
        1,
        "user",
        "completed",
        "native-input",
        None,
        false,
    )
    .await;
    let turn = backend
        .state
        .store
        .get_turn("large-0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(turn.request_text, "user native-input");
    let page = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    assert_eq!(page.nodes.len(), 1);
    assert_eq!(page.nodes[0].id, "p:large-0");
    assert_eq!(
        backend
            .state
            .store
            .node_window(&id, 1)
            .await
            .unwrap()
            .target_id
            .as_deref(),
        Some("p:large-0")
    );
}

#[tokio::test]
async fn a_completed_thinking_block_rejects_late_deltas_but_accepts_completion_corrections() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    for (seq, phase) in [(0, "started"), (1, "completed"), (2, "delta")] {
        append(
            &backend, &id, seq, "thinking", phase, "thought", None, false,
        )
        .await;
    }
    let page = backend
        .state
        .store
        .node_page(&id, "g:0", None, false, 40)
        .await
        .unwrap();
    assert_eq!(page.nodes[0].running, 0);
    assert_eq!(page.nodes[0].event.phase.as_deref(), Some("completed"));
    assert_eq!(page.nodes[0].event.text, "thinking thought");
    append(
        &backend,
        &id,
        3,
        "thinking",
        "completed",
        "thought",
        None,
        false,
    )
    .await;
    let page = backend
        .state
        .store
        .node_page(&id, "g:0", None, false, 40)
        .await
        .unwrap();
    assert_eq!(page.nodes[0].event.seq, 3);
    assert_eq!(page.nodes[0].running, 0);
}

#[tokio::test]
async fn visible_reply_search_is_complete_before_during_and_after_archiving() {
    let backend = TestBackend::start().await;
    let id = seed_with_text(
        &backend,
        1,
        format!("{} searchablequartz", "prefix ".repeat(1000)),
    )
    .await;
    for stage in 0..3 {
        if stage == 1 {
            let events = backend
                .state
                .store
                .turn_events_for_consolidation("large-1")
                .await
                .unwrap();
            backend
                .state
                .history
                .put_blocks(&sylph_server::storage::history::consolidate(&events))
                .await
                .unwrap();
        } else if stage == 2 {
            sylph_server::storage::history::consolidate_turn(
                &backend.state.store,
                &backend.state.history,
                "large-1",
            )
            .await
            .unwrap();
        }
        let response = backend
            .get_authed(&format!(
                "/api/v1/sessions/{id}/nodes/search?q=searchablequartz"
            ))
            .await;
        let body = common::json_body(response).await;
        assert_eq!(
            body["hits"].as_array().unwrap().len(),
            1,
            "archive stage {stage}: {body}"
        );
        assert_eq!(body["hits"][0]["seq"], 0);
    }
}

#[tokio::test]
async fn codex_live_commands_contribute_concrete_activity_and_command_counts() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 1).await;
    let mut event = backend
        .state
        .store
        .turn_events_for_consolidation("large-1")
        .await
        .unwrap()
        .remove(0);
    event.id = None;
    event.seq = 1;
    event.kind = "tool_call".into();
    event.phase = Some("started".into());
    event.stream_id = Some("command".into());
    event.name = Some("exec".into());
    event.text = "cargo test --workspace".into();
    event.data_json = json!({"command":"cargo test --workspace","cwd":"/workspace"}).to_string();
    backend.state.store.append_event(&event).await.unwrap();
    let page = backend
        .state
        .store
        .node_page(&id, "", None, false, 40)
        .await
        .unwrap();
    let group = page
        .nodes
        .iter()
        .find(|n| n.event.kind == "execution")
        .unwrap();
    assert_eq!(group.commands, 1);
    assert_eq!(group.activity, "Running cargo test --workspace");
}

#[tokio::test]
async fn real_adapter_tool_events_share_header_and_counter_semantics() {
    use futures::TryStreamExt;
    use sylph_protocol::{Adapter, NativeRecord, SessionMessage};
    let fixtures: Vec<(Box<dyn Adapter>, Value)> = vec![
        (
            Box::new(sylph_adapters::codex::CodexAdapter),
            json!({"method":"item/started","params":{"threadId":"test","turnId":"turn","item":{"type":"commandExecution","id":"exec","command":"cargo test --workspace","cwd":"/workspace"}}}),
        ),
        (
            Box::new(sylph_adapters::claude::ClaudeAdapter),
            json!({"type":"assistant","message":{"id":"assistant","content":[{"type":"tool_use","id":"exec","name":"Bash","input":{"command":"cargo test --workspace"}}]}}),
        ),
        (
            Box::new(sylph_adapters::pi::PiAdapter),
            json!({"type":"tool_execution_start","toolCallId":"exec","toolName":"bash","args":{"command":"cargo test --workspace"}}),
        ),
    ];
    for (adapter, payload) in fixtures {
        let backend = TestBackend::start().await;
        let id = seed(&backend, 0).await;
        let messages: Vec<_> = adapter
            .reproject(vec![NativeRecord {
                source: "live/stdout/fixture.jsonl".into(),
                version: 1,
                raw_json: None,
                payload,
                blobs: vec![],
            }])
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        let mut calls = 0;
        for message in messages {
            if let SessionMessage::Event(event) = message {
                if event.kind != sylph_protocol::EventKind::ToolCall {
                    continue;
                }
                calls += 1;
                let mut event = sylph_server::events::StreamEvent::from_protocol(&event)
                    .to_row(&SessionId::decode(&id).unwrap(), "large-0");
                event.seq = calls;
                backend.state.store.append_event(&event).await.unwrap();
            }
        }
        assert!(calls > 0, "the real adapter emitted no tool calls");
        let roots = backend
            .state
            .store
            .node_page(&id, "", None, false, 40)
            .await
            .unwrap();
        let group = roots
            .nodes
            .iter()
            .find(|node| node.event.kind == "execution")
            .unwrap();
        assert_eq!(group.commands, 1);
        assert_eq!(group.activity, "Running cargo test --workspace");
        let children = backend
            .state
            .store
            .node_page(&id, &group.id, None, false, 40)
            .await
            .unwrap();
        let summary = children.nodes[0].event.tool_summary.as_ref().unwrap();
        assert_eq!(summary.kind, "shell");
        assert_eq!(summary.command.as_deref(), Some("cargo test --workspace"));
        assert_eq!(summary.summary, "cargo test --workspace");
    }
}

#[tokio::test]
async fn search_matches_across_deltas_and_excludes_superseded_body_revisions() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 1).await;
    let mut event = backend
        .state
        .store
        .turn_events_for_consolidation("large-1")
        .await
        .unwrap()
        .remove(0);
    event.id = None;
    event.kind = "assistant".into();
    event.stream_id = Some("reply".into());
    for (seq, phase, text) in [
        (1, "started", ""),
        (2, "delta", "crossdelta"),
        (3, "delta", "quartz"),
    ] {
        event.seq = seq;
        event.phase = Some(phase.into());
        event.text = text.into();
        backend.state.store.append_event(&event).await.unwrap();
    }
    let search = format!("/api/v1/sessions/{id}/nodes/search?q=crossdeltaquartz");
    let body = common::json_body(backend.get_authed(&search).await).await;
    assert_eq!(body["hits"].as_array().unwrap().len(), 1);
    assert_eq!(body["hits"][0]["seq"], 3);
    sylph_server::storage::history::consolidate_turn(
        &backend.state.store,
        &backend.state.history,
        "large-1",
    )
    .await
    .unwrap();
    event.seq = 4;
    event.phase = Some("completed".into());
    event.text = "corrected answer".into();
    backend.state.store.append_event(&event).await.unwrap();
    for archive in [false, true] {
        if archive {
            sylph_server::storage::history::consolidate_turn(
                &backend.state.store,
                &backend.state.history,
                "large-1",
            )
            .await
            .unwrap();
        }
        let body = common::json_body(backend.get_authed(&search).await).await;
        assert!(
            body["hits"].as_array().unwrap().is_empty(),
            "obsolete body matched: {body}"
        );
    }
}

#[tokio::test]
async fn search_includes_the_current_report_of_a_promoted_tool() {
    let backend = TestBackend::start().await;
    let id = seed(&backend, 0).await;
    append(
        &backend,
        &id,
        0,
        "tool_call",
        "completed",
        "worker",
        None,
        false,
    )
    .await;
    append(
        &backend, &id, 1, "subagent", "started", "worker", None, false,
    )
    .await;
    append(
        &backend,
        &id,
        2,
        "subagent",
        "completed",
        "worker",
        None,
        false,
    )
    .await;
    for archive in [false, true] {
        if archive {
            sylph_server::storage::history::consolidate_turn(
                &backend.state.store,
                &backend.state.history,
                "large-0",
            )
            .await
            .unwrap();
        }
        let body = common::json_body(
            backend
                .get_authed(&format!("/api/v1/sessions/{id}/nodes/search?q=subagent"))
                .await,
        )
        .await;
        assert_eq!(
            body["hits"].as_array().unwrap().len(),
            1,
            "promoted report disappeared, archive={archive}: {body}"
        );
    }
}
