//! Browse the durable TraceDB archive by working directory.
//!
//! `/search` answers "which archived session mentioned X", which only helps a
//! user who already knows what to look for. These endpoints answer the question
//! the sidebar actually asks: "what happened in this directory before?" Without
//! them a freshly pinned workspace reads as empty while the archive holds
//! hundreds of sessions for that exact path.
//!
//! **Reading is not importing.** `POST /sessions/import` adopts a session for
//! continuation: it writes a row, requires a runner for the originating agent,
//! and requires the recorded `cwd` to still exist. None of that is needed to
//! read history, and all of it fails for exactly the old sessions a user most
//! wants to look back at. So these endpoints are read-only and mutate nothing;
//! import stays an explicit action taken from the archived view.
//!
//! Two properties of TraceDB's `list` shape this module, both verified against
//! the local archive rather than assumed:
//!
//! * `cwd` is a **substring** filter upstream, so a request for
//!   `/w/app` also returns `/w/app-old` and every subdirectory. Rows are
//!   re-filtered here for exact equality.
//! * `list` does **not** collapse fork lineage, though `search` does. Subagent
//!   sessions are 43% of the local archive (2018 of 4700), so listing them as
//!   peers would bury the sessions a user actually started. They are folded into
//!   their parent as a count.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::workspaces::normalize;
use crate::error::{Error, Result};
use crate::events::StreamEvent;
use crate::tracedb::{split_lineage, TraceEvent};
use crate::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    /// Absolute directory to list history for. Required in effect: an unfiltered
    /// archive listing is the `/search` page's job, not this endpoint's.
    ///
    /// Optional in the struct so a missing value fails through this module's own
    /// validation and answers the documented `{error: {code, message}}` shape.
    /// Making it non-optional lets axum's query rejection answer first, with a
    /// bare `Failed to deserialize query string` body that no client can branch
    /// on.
    workdir: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    /// Opaque cursor from a previous response's `nextCursor`.
    cursor: Option<String>,
}

fn default_limit() -> usize {
    20
}

/// How many upstream pages one request may walk.
///
/// Exact-path filtering and lineage rollup both shrink a page, so reaching the
/// requested `limit` usually takes more than one upstream call. This bounds that
/// loop: without it, a request for a directory with few sessions would page
/// through the whole 4700-row archive looking for matches it will never find.
const MAX_UPSTREAM_PAGES: usize = 8;

/// Upstream page size. Larger than the typical `limit` because most rows are
/// discarded — wrong directory, subagent, or empty.
const UPSTREAM_PAGE: usize = 100;

/// One archived session, as the sidebar and history browser render it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveSession {
    /// TraceDB session id (`claude:uuid`), not a Sylph Web session id.
    pub id: String,
    pub agent: String,
    /// Recognizable label: the archive's title, else the opening typed message.
    /// Absent only when the session recorded neither, which measurement puts at
    /// roughly 2% of real project sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<i64>,
    /// Events the archive holds, which is the only size signal available.
    pub events: i64,
    /// Subagent sessions spawned by this one, folded in rather than listed.
    pub subagent_count: i64,
    /// Live session id when this archived session was already imported, so the
    /// UI opens the live conversation instead of showing it twice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveListResponse {
    pub sessions: Vec<ArchiveSession>,
    /// Cursor to continue from, absent when the archive has no more rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// False when TraceDB could not be reached. The request still succeeds.
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// GET /api/v1/archive/sessions?workdir={path}&limit={n}&cursor={c}
///
/// An unreachable archive answers 200 with `available: false`, matching
/// `/search`: TraceDB is optional locally, and a sidebar that renders an error
/// banner because an optional component is absent is worse than one that quietly
/// shows only live sessions.
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<ListQuery>,
) -> Result<Json<ArchiveListResponse>> {
    // Validated before normalizing: `normalize("")` is `"/"`, so normalizing
    // first would turn a missing parameter into a listing of the root directory.
    let requested = params.workdir.as_deref().unwrap_or_default().trim();
    if requested.is_empty() || !requested.starts_with('/') {
        return Err(Error::InvalidRequest(
            "workdir is required and must be an absolute path.".to_string(),
        ));
    }
    let workdir = normalize(requested);

    let limit = params.limit.clamp(1, 100);
    let mut cursor = params.cursor.clone();
    let mut rows: Vec<ArchiveSession> = Vec::new();
    // Root id -> index in `rows`, so a child found after its parent increments
    // the right row.
    let mut by_root: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    // Children whose parent has not been seen yet. The archive orders by recency,
    // and a subagent commonly ends after the session that spawned it, so its row
    // can arrive first.
    let mut orphan_children: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();

    for _ in 0..MAX_UPSTREAM_PAGES {
        let page = match state
            .tracedb
            .list(Some(&workdir), UPSTREAM_PAGE, cursor.as_deref())
            .await
        {
            Ok(page) => page,
            Err(detail) => {
                tracing::warn!("TraceDB list failed: {}", detail);
                return Ok(Json(ArchiveListResponse {
                    sessions: Vec::new(),
                    next_cursor: None,
                    available: false,
                    detail: Some(detail),
                }));
            }
        };

        let exhausted = page.next_cursor.is_none();
        cursor = page.next_cursor;

        for summary in page.sessions {
            // The upstream filter is a substring match, so a request for
            // `/w/app` also returns `/w/app-old` and `/w/app/sub`.
            let Some(cwd) = summary.cwd.as_deref().map(normalize) else {
                continue;
            };
            if cwd != workdir {
                continue;
            }
            // Ingested but empty: common for aborted runs, and there is nothing
            // to read.
            if summary.events == 0 {
                continue;
            }

            if let Some((root, _)) = split_lineage(&summary.id) {
                match by_root.get(root) {
                    Some(&at) => rows[at].subagent_count += 1,
                    None => *orphan_children.entry(root.to_string()).or_insert(0) += 1,
                }
                continue;
            }

            if rows.len() >= limit {
                break;
            }

            let label = state
                .tracedb
                .session_label(&summary.id, summary.title.as_deref())
                .await;

            let session_id = state
                .store
                .find_session_by_trace_id(&summary.id)
                .await?
                .map(|session| session.id);

            by_root.insert(summary.id.clone(), rows.len());
            rows.push(ArchiveSession {
                subagent_count: orphan_children.remove(&summary.id).unwrap_or(0),
                id: summary.id,
                agent: summary.agent,
                label,
                cwd,
                model: summary.model,
                started_at_ms: summary.started_at_ms,
                ended_at_ms: summary.ended_at_ms,
                events: summary.events,
                session_id,
            });
        }

        if rows.len() >= limit || exhausted {
            if exhausted {
                cursor = None;
            }
            break;
        }
    }

    Ok(Json(ArchiveListResponse {
        sessions: rows,
        next_cursor: cursor,
        available: true,
        detail: None,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptQuery {
    #[serde(default = "default_event_limit")]
    limit: usize,
}

fn default_event_limit() -> usize {
    2000
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptResponse {
    pub id: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Recorded working directory. Absent for 19% of the archive, and its
    /// absence is why a session cannot be continued.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    /// Normalized into the same shape the live timeline renders.
    pub events: Vec<StreamEvent>,
    /// True when `limit` cut the stream short.
    pub truncated: bool,
    /// Live session id when already imported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// GET /api/v1/archive/sessions/{trace_id}
///
/// `trace_id` is captured greedily: a subagent id contains a `/`
/// (`claude:uuid/agent-name`), so a single-segment capture would reject 43% of
/// the archive.
pub async fn transcript(
    State(state): State<AppState>,
    Path(trace_id): Path<String>,
    Query(params): Query<TranscriptQuery>,
) -> Result<Json<TranscriptResponse>> {
    let trace_id = trace_id.trim();
    if trace_id.is_empty() {
        return Err(Error::InvalidRequest(
            "An archived session id is required.".to_string(),
        ));
    }

    let limit = params.limit.clamp(1, 5000);

    let trace = state
        .tracedb
        .show(trace_id, None)
        .await
        .map_err(Error::TraceDbUnavailable)?
        .ok_or_else(|| Error::TraceSessionNotFound(trace_id.to_string()))?;

    // Sliced before normalizing, so `limit` bounds the work done rather than only
    // the rows returned. The count compares raw events because that is what the
    // slice applies to.
    let truncated = trace.events.len() > limit;
    let events = normalize_events(&trace.events[..trace.events.len().min(limit)]);

    let label = state
        .tracedb
        .session_label(trace_id, trace.session.title.as_deref())
        .await;

    let session_id = state
        .store
        .find_session_by_trace_id(trace_id)
        .await?
        .map(|session| session.id);

    Ok(Json(TranscriptResponse {
        id: trace.session.id,
        agent: trace.session.agent,
        label,
        cwd: trace.session.cwd,
        started_at_ms: trace.session.started_at_ms,
        events,
        truncated,
        session_id,
    }))
}

/// True when an archived event carries no reading value.
///
/// The archive records each agent's internal bookkeeping as `system` events:
/// `mode`, `permission-mode`, `task_started`, `world_state`,
/// `thread_settings_applied`, `model_change`, and so on. Their `text` is a raw
/// JSON blob of harness configuration, and one real session carries dozens —
/// 49 of 419 events in a measured case. The live runner protocol emits no
/// equivalent, so the timeline has no renderer for them and falls back to
/// printing the JSON, which buries the conversation in machine state.
///
/// Dropped here rather than filtered in the timeline: this is a property of the
/// archive's vocabulary, and the timeline should keep rendering exactly the
/// shape the live protocol defines.
fn is_bookkeeping(event: &TraceEvent) -> bool {
    event.kind.as_deref() == Some("system")
}

/// Adapt archived events into the live `StreamEvent` shape.
///
/// The two vocabularies already agree on event kinds (`user`, `assistant`,
/// `thinking`, `tool_call`, `tool_result`), so the timeline renders archived
/// history without a second code path. Four details need translating:
///
/// * **Phase.** Archived events are settled by definition, so every one is
///   `completed`. That also stops `buildRows` from treating them as an open
///   streaming block awaiting more text.
/// * **Bookkeeping.** Agent-internal `system` events are dropped; see
///   [`is_bookkeeping`].
/// * **Tool arguments.** `lib/tool-shape.ts` reads `data.input` with `text` as a
///   fallback; the archive puts the arguments JSON in `text`. Parsing it into
///   `data.input` keeps diffs and command rendering working, and leaving `text`
///   in place preserves the fallback when it is not JSON.
/// * **Pairing.** The frontend groups a result with its call by `streamId`, and
///   the two agents record that link differently: `codex` repeats the same
///   `call_id` on both, while `claude` points the result's `parent_id` at the
///   call's `native_id`. Both are mapped onto one `streamId` so grouping works
///   for either.
fn normalize_events(events: &[TraceEvent]) -> Vec<StreamEvent> {
    // native_id -> the call's stream id, for agents that link by parent_id.
    let mut call_streams: std::collections::HashMap<&str, String> =
        std::collections::HashMap::new();

    let mut out = Vec::with_capacity(events.len());

    for (position, event) in events.iter().enumerate() {
        if is_bookkeeping(event) {
            continue;
        }

        let kind = event.kind.clone().unwrap_or_else(|| "status".to_string());
        let seq = event.idx.unwrap_or(position as i64) as i32;

        let stream_id = match kind.as_str() {
            "tool_call" => {
                let id = event
                    .call_id
                    .clone()
                    .or_else(|| event.native_id.clone())
                    .unwrap_or_else(|| format!("call-{seq}"));
                if let Some(native) = event.native_id.as_deref() {
                    call_streams.insert(native, id.clone());
                }
                Some(id)
            }
            "tool_result" => event
                .call_id
                .clone()
                .or_else(|| {
                    event
                        .parent_id
                        .as_deref()
                        .and_then(|parent| call_streams.get(parent).cloned())
                })
                .or_else(|| event.parent_id.clone()),
            _ => None,
        };

        // Tool arguments are a JSON object in `text`; the timeline expects them
        // under `data.input`. Non-JSON text is left alone, which the frontend
        // already handles as its documented fallback.
        let data = if kind == "tool_call" {
            event
                .text
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .map(|input| serde_json::json!({ "input": input }))
        } else {
            None
        };

        out.push(StreamEvent {
            seq,
            kind,
            // Archived events are settled history, never mid-stream.
            phase: Some("completed".to_string()),
            stream_id,
            name: event.name.clone(),
            text: event.text.clone().unwrap_or_default(),
            data,
            is_error: event.is_error.unwrap_or(false),
            stability: "stable".to_string(),
            source: "archive".to_string(),
            created_at_ms: event.created_at_ms.unwrap_or(0),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace_event(kind: &str, text: Option<&str>) -> TraceEvent {
        TraceEvent {
            idx: None,
            kind: Some(kind.to_string()),
            text: text.map(str::to_string),
            created_at_ms: Some(7),
            name: None,
            call_id: None,
            parent_id: None,
            native_id: None,
            is_error: None,
            subtype: None,
        }
    }

    #[test]
    fn agent_bookkeeping_is_dropped_rather_than_rendered_as_json() {
        // Every `system` subtype in the archive is harness configuration whose
        // text is a raw JSON blob: mode, permission-mode, task_started,
        // world_state, model_change. The live protocol emits no equivalent, so
        // the timeline would print them verbatim.
        let mut mode = trace_event("system", Some(r#"{"mode":"normal"}"#));
        mode.subtype = Some("mode".to_string());
        let mut world = trace_event("system", Some(r#"{"full":true,"state":{}}"#));
        world.subtype = Some("world_state".to_string());

        let events = normalize_events(&[
            mode,
            trace_event("user", Some("fix the bug")),
            world,
            trace_event("assistant", Some("on it")),
        ]);

        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["user", "assistant"], "bookkeeping leaked into the transcript");
    }

    #[test]
    fn an_unknown_event_kind_still_renders_as_a_status_line() {
        // A future agent may record a kind this build has never seen. The
        // timeline's fallback is `status`, which prints it plainly rather than
        // dropping content.
        let events = normalize_events(&[trace_event("some_future_kind", Some("hello"))]);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "some_future_kind");
    }

    #[test]
    fn archived_events_are_marked_settled_so_the_timeline_does_not_await_more() {
        let events = normalize_events(&[trace_event("assistant", Some("done"))]);

        assert_eq!(events[0].phase.as_deref(), Some("completed"));
        assert_eq!(events[0].stability, "stable");
        assert_eq!(events[0].source, "archive");
    }

    #[test]
    fn tool_arguments_move_into_data_input_for_the_timeline() {
        let mut call = trace_event("tool_call", Some(r#"{"command":"ls -la"}"#));
        call.name = Some("Bash".to_string());
        call.call_id = Some("call_1".to_string());

        let events = normalize_events(&[call]);

        assert_eq!(events[0].name.as_deref(), Some("Bash"));
        assert_eq!(
            events[0].data.as_ref().and_then(|d| d.get("input")).and_then(|i| i.get("command")),
            Some(&serde_json::json!("ls -la")),
        );
        // The raw text stays, because it is the documented fallback.
        assert!(events[0].text.contains("ls -la"));
    }

    #[test]
    fn non_json_tool_text_is_left_as_text() {
        let mut call = trace_event("tool_call", Some("not json at all"));
        call.call_id = Some("c".to_string());

        let events = normalize_events(&[call]);

        assert!(events[0].data.is_none(), "nothing to parse means no data");
        assert_eq!(events[0].text, "not json at all");
    }

    #[test]
    fn codex_pairs_a_result_to_its_call_by_shared_call_id() {
        // Observed: codex repeats one `call_id` on both events.
        let mut call = trace_event("tool_call", Some("{}"));
        call.call_id = Some("call_3T9Y".to_string());
        let mut result = trace_event("tool_result", Some("output"));
        result.call_id = Some("call_3T9Y".to_string());

        let events = normalize_events(&[call, result]);

        assert_eq!(events[0].stream_id, events[1].stream_id);
        assert_eq!(events[0].stream_id.as_deref(), Some("call_3T9Y"));
    }

    #[test]
    fn claude_pairs_a_result_to_its_call_through_parent_id() {
        // Observed: claude leaves `call_id` unset on the result and points
        // `parent_id` at the call's `native_id`.
        let mut call = trace_event("tool_call", Some("{}"));
        call.native_id = Some("native-abc".to_string());
        call.call_id = Some("toolu_bdrk_1".to_string());
        let mut result = trace_event("tool_result", Some("output"));
        result.parent_id = Some("native-abc".to_string());

        let events = normalize_events(&[call, result]);

        assert_eq!(
            events[0].stream_id, events[1].stream_id,
            "a result must group with its call",
        );
    }

    #[test]
    fn an_unpaired_tool_call_still_gets_a_stream_id() {
        // Some archived calls carry neither id. They must still render, and must
        // not collide with each other.
        let events = normalize_events(&[
            trace_event("tool_call", Some("{}")),
            trace_event("tool_call", Some("{}")),
        ]);

        assert_ne!(events[0].stream_id, events[1].stream_id);
        assert!(events[0].stream_id.is_some());
    }
}
