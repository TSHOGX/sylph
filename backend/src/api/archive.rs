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
use crate::tracedb::{split_lineage, Lineage, TraceEvent};
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

/// How many upstream rows one request may examine.
///
/// Exact-path filtering and lineage rollup both shrink a page, so reaching the
/// requested `limit` usually takes more than one upstream call. This bounds that
/// loop so a request cannot walk the archive indefinitely.
///
/// Counted in rows rather than pages, and generous, because the upstream `cwd`
/// filter is an unanchored substring match: a parent directory's pattern also
/// matches every path beneath it, and those sort by recency, not by depth. For
/// `/Users/x/Workspace/huawei` — 88 sessions of its own — the first exact match
/// lands 1300 rows in, behind subdirectory work. An earlier 8-page cap gave up
/// before reaching it and reported the directory as empty, which is the same
/// wrong answer this endpoint exists to fix.
///
/// The archive is local SQLite and each page is one pipe round trip, so the cost
/// of the ceiling is bounded and only paid by directories that actually need it.
const MAX_UPSTREAM_ROWS: usize = 4000;

/// Upstream page size.
///
/// 500 because that is what `trace-db` returns at most, whatever larger value is
/// requested — asking for more buys nothing and hides the real page size from
/// anyone reading this. Larger than the typical `limit` because most rows are
/// discarded: wrong directory, subagent, or empty.
const UPSTREAM_PAGE: usize = 500;

/// How many rows the per-transcript delegation scan may examine.
///
/// Much larger than [`MAX_UPSTREAM_ROWS`], and for a reason that is about what the
/// two ceilings mean rather than about generosity. That one bounds a *listing*,
/// which is polled and which can honestly answer "here is a page, ask for more".
/// This one bounds a search for the children of one session: there is no next page
/// to defer to, so stopping early means reporting a session as having fewer
/// delegates than it has.
///
/// Sized against the measured archive, not guessed: a full unfiltered pass is 4922
/// rows in 10 pages at 164 ms, so 25000 rows is roughly 830 ms in the worst case
/// and is paid only when someone deliberately opens a transcript. The ceiling
/// exists so the request cannot become unbounded, not as a routine cut — an
/// archive that reaches it gets `delegationsTruncated`, which is then a real signal
/// instead of a constant.
const MAX_DELEGATION_SCAN_ROWS: usize = 25_000;

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
    /// The folded subagents themselves, in the order the archive reported them.
    ///
    /// A count alone is a dead end: the archive knows *which* sessions were
    /// delegated — half of it is delegated work — and a user who reads "13
    /// subagents" has no way to open any of them. The fold already visits every
    /// child to count it, so reporting identity here costs nothing beyond the
    /// bytes.
    ///
    /// Session-level, deliberately. See [`normalize_events`]: the archive records
    /// no event-level parent pointer, so nothing here says *where* in the parent's
    /// transcript a delegation happened, and the client must not imply that it
    /// does.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<ArchiveDelegate>,
    /// Set when this session is a fork of another: the origin's TraceDB id.
    ///
    /// A fork replays its origin's whole prefix, so the two read as one
    /// conversation and only one belongs in a listing. Within a single response
    /// the loser is already dropped, but a pair can straddle two pages — one
    /// measured pair sits 268 rows apart — and the server holds no state between
    /// requests. Reporting the relationship lets a client that accumulates pages
    /// suppress the duplicate, which is where cross-page state naturally lives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_of: Option<String>,
    /// Live session id when this archived session was already imported, so the
    /// UI opens the live conversation instead of showing it twice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// One delegated session, as its parent's row reports it.
///
/// Deliberately thinner than [`ArchiveSession`]: this is a row in a disclosure
/// under its parent, not a listing of its own, and every field here comes from
/// the `list` page the fold was already reading. In particular there is no
/// `label` fallback to the opening user turn — that costs a second request per
/// child, and one measured parent has 193 of them. `title` is whatever the
/// archive recorded, which for `claude` is most sessions and for `codex` and `pi`
/// is none; a client that wants more opens the child.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveDelegate {
    /// TraceDB session id, including the `/agent-…` suffix when it has one.
    pub id: String,
    pub agent: String,
    /// The archive's own title. Absent for the agents that record none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Recorded working directory, when it differs from the parent's.
    ///
    /// Present only when different, because equal is the common case and the
    /// difference is the interesting one: 484 of 2238 subagents in the local
    /// archive ran somewhere else, typically a worktree. A child listed under its
    /// parent while having run elsewhere is worth saying so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub events: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<i64>,
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

/// Whether a parent session belongs in the directory being listed.
///
/// Decides whether a subagent folds into its parent or stands as its own row. A
/// parent is "in this directory" only when the archive records the same
/// normalized `cwd`; otherwise the child is the only trace of that work in the
/// directory it actually ran in, and folding it away hides it entirely.
///
/// A lookup failure answers `false`, which lists the child. Between a duplicated
/// row and a vanished session, the duplicate is the recoverable mistake.
async fn parent_in_directory(state: &AppState, parent: &str, workdir: &str) -> bool {
    match state.tracedb.session_metadata(parent).await {
        Ok(Some(session)) => session.cwd.as_deref().map(normalize).as_deref() == Some(workdir),
        Ok(None) => false,
        Err(detail) => {
            report_tracedb_failure(
                state,
                &format!("Parent lookup failed for {}", parent),
                &detail,
            );
            false
        }
    }
}

/// Build a delegate row from the `list` summary the fold already holds.
///
/// `cwd` is the child's normalized directory and `parent_cwd` the parent's;
/// the field is reported only when they differ, so an ordinary same-directory
/// delegation carries no redundant path.
fn delegate_of(
    summary: &crate::tracedb::SessionSummary,
    cwd: &str,
    parent_cwd: &str,
) -> ArchiveDelegate {
    ArchiveDelegate {
        id: summary.id.clone(),
        agent: summary.agent.clone(),
        title: summary
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(crate::tracedb::summarize_line),
        cwd: (cwd != parent_cwd).then(|| cwd.to_string()),
        events: summary.events,
        started_at_ms: summary.started_at_ms,
        ended_at_ms: summary.ended_at_ms,
    }
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
    // Parent id -> index in `rows`, so a child found after its parent increments
    // the right row.
    let mut by_parent: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    // Subagents of a parent not yet seen. The archive orders newest-first and a
    // subagent commonly ends after the session that spawned it, so a child's row
    // often arrives before its parent's.
    let mut pending_children: std::collections::HashMap<String, Vec<ArchiveDelegate>> =
        std::collections::HashMap::new();
    // Sessions folded away, so a fork's other side is recognized whichever order
    // the two arrive in, and a child is never also listed as a row.
    let mut folded: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Parent id -> whether it lives in this directory, so a parent with many
    // children costs one lookup rather than one per child.
    let mut parent_locality: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();

    let mut examined = 0usize;

    while examined < MAX_UPSTREAM_ROWS {
        let page = match state
            .tracedb
            .list(Some(&workdir), UPSTREAM_PAGE, cursor.as_deref())
            .await
        {
            Ok(page) => page,
            Err(detail) => {
                // Loud once, quiet after — see `report_tracedb_failure`. An absent
                // archive is a supported configuration, so it is one fact about the
                // install rather than one fault per request. Measured in the E2E log
                // before this bound existed: ~30 identical lines per page load,
                // because the sidebar and the history overlay each list several
                // directories, and 2339 in a full run.
                //
                // Demoting *every* occurrence to `debug` was the other candidate and
                // is worse: it also hides the first failure, which is the one that
                // tells an operator their archive is misconfigured. The response
                // carries `available: false` and `detail` either way, so the client
                // renders the real reason regardless of the log level.
                report_tracedb_failure(&state, "TraceDB list failed", &detail);
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
        examined += page.sessions.len();

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
            // Already accounted for as the other side of a fork.
            if folded.contains(&summary.id) {
                continue;
            }

            // Lineage comes from the archive's own record, not from the id shape.
            // `list` reports none, so this costs one cheap metadata call per
            // candidate — the alternative, reading the `/agent-` suffix, misses the
            // 199 sessions that record only a `parent_session_id` and leaves
            // delegated work sitting at the top level.
            //
            // A failed lookup degrades to the id suffix rather than dropping the
            // row: an unrollable session is a cosmetic problem, a vanished one is
            // not.
            let metadata = state.tracedb.session_metadata(&summary.id).await;
            if let Err(detail) = &metadata {
                report_tracedb_failure(
                    &state,
                    &format!("Lineage lookup failed for {}", summary.id),
                    detail,
                );
            }
            let metadata = metadata.ok().flatten();
            let lineage = metadata
                .as_ref()
                .map(|session| session.lineage())
                .unwrap_or_else(|| match split_lineage(&summary.id) {
                    Some((parent, _)) => Lineage::Subagent { parent },
                    None => Lineage::Root,
                });

            // Recorded on the row when this session is a fork, so a client holding
            // several pages can drop a duplicate whose pair fell in another
            // request. Within this request the pair is already resolved above.
            let mut fork_of: Option<String> = None;

            match lineage {
                Lineage::Subagent { parent } => {
                    // Delegated work folds into its parent — but only when the
                    // parent is a row in *this* directory. 484 of the 2238
                    // subagents in the local archive ran in a different directory
                    // than the session that spawned them, typically a worktree or
                    // a subdirectory, and folding those unconditionally deletes
                    // them from the only listing that would ever show them. It
                    // emptied 42 directories, one of them holding 193 sessions.
                    //
                    // So a cross-directory child is promoted to a row of its own:
                    // in its own directory it is not delegated work, it is the
                    // work. Held aside when the parent has not been seen yet,
                    // because the archive orders newest-first and a subagent
                    // commonly outlives its parent.
                    if let Some(&at) = by_parent.get(parent) {
                        rows[at].subagent_count += 1;
                        let delegate = delegate_of(&summary, &cwd, &rows[at].cwd);
                        rows[at].subagents.push(delegate);
                        continue;
                    }
                    // Cached: one parent commonly has dozens of children — 193 in
                    // the largest local case — and they all ask the same question.
                    let local = match parent_locality.get(parent) {
                        Some(&known) => known,
                        None => {
                            let known = parent_in_directory(&state, parent, &workdir).await;
                            parent_locality.insert(parent.to_string(), known);
                            known
                        }
                    };
                    if local {
                        // `local` is exactly "the parent's recorded cwd is this
                        // workdir", so the parent's directory is known here without
                        // having seen its row yet.
                        let delegate = delegate_of(&summary, &cwd, &workdir);
                        pending_children
                            .entry(parent.to_string())
                            .or_default()
                            .push(delegate);
                        continue;
                    }
                    // Falls through and is listed.
                }
                Lineage::Fork { origin } => {
                    // The same conversation resumed: both sides replay an identical
                    // prefix, so listing both shows one conversation twice. Keep
                    // this one and fold the other away.
                    //
                    // Whichever side is newer must survive, and that is not a fixed
                    // side: of the two real cases in the local archive, one has its
                    // later work in the fork (1108 events against 389) and the
                    // other in the origin (2459 against 649). `list` is ordered by
                    // `endedAtMs` descending, so the newer side is simply the one
                    // reached first — no comparison needed, and the loser is
                    // recorded so it is skipped when it arrives.
                    folded.insert(origin.to_string());
                    if by_parent.contains_key(origin) {
                        // The origin is already a row, so it ran more recently.
                        // Leave it standing and drop this side.
                        continue;
                    }
                    fork_of = Some(origin.to_string());
                }
                Lineage::Root => {}
            }

            if rows.len() >= limit {
                break;
            }

            // Reuses the title from the metadata call just made, so labelling a row
            // adds a request only when the archive recorded no title.
            let title = metadata
                .as_ref()
                .and_then(|session| session.title.as_deref())
                .or(summary.title.as_deref());
            let label = state.tracedb.session_label(&summary.id, title).await;

            let session_id = state
                .store
                .find_session_by_archive_id(&summary.id)
                .await?
                .map(|session| session.id);

            let subagents = pending_children.remove(&summary.id).unwrap_or_default();

            by_parent.insert(summary.id.clone(), rows.len());
            rows.push(ArchiveSession {
                subagent_count: subagents.len() as i64,
                subagents,
                fork_of,
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
    /// Sessions this one delegated to, which is the downward half of the tree.
    ///
    /// Found by one pass over `list` bounded by [`MAX_DELEGATION_SCAN_ROWS`],
    /// because TraceDB offers no parent
    /// filter — the ops are `stats`, `search`, `list`, `show`, `reconstruct`, and
    /// only `show` reports lineage, per session. Measured against the local
    /// archive: a full unfiltered pass is 4922 rows in 10 pages at **164 ms**, and
    /// building the child index from it costs 1 ms. A transcript is opened
    /// deliberately rather than polled, so that is affordable where it would not
    /// be on the sidebar's path.
    ///
    /// **Complete for delegates whose id carries the parent as a prefix, and
    /// deliberately not otherwise.** 2104 of the archive's 2316 delegates are
    /// named `<parent>/agent-…`, so a prefix test finds them from the listing
    /// alone. The remaining 212 — almost all `opencode` — record only a
    /// `parent_session_id`, which is visible only to `show`: finding them means a
    /// metadata call per candidate row, measured at 11.5 ms each and **52.9 s**
    /// across the archive. That is not a request anyone can wait on, so those
    /// delegates are reached from their parent's *directory listing*, where the
    /// fold already reads their metadata for a different reason.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub delegations: Vec<ArchiveDelegate>,
    /// True when the delegation scan hit its row ceiling, so `delegations` may be
    /// short. Distinguishes "no delegates" from "did not finish looking".
    pub delegations_truncated: bool,
    /// The session that delegated this one, when it is delegated work.
    ///
    /// Free: `show` already reports it, so this needs no extra request. It is the
    /// upward half of the session-level tree — a delegate opened from its parent's
    /// row can get back, and one opened from a search hit can find out it is not a
    /// conversation someone started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// The session this one was forked or resumed from.
    ///
    /// A different relationship from `parentId` and not a delegation: a fork
    /// replays its origin's whole prefix, so the two read as one conversation and
    /// the link is what explains a transcript that opens mid-conversation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
    /// True when the archive records no event-level delegation structure for this
    /// agent, which is every agent measured.
    ///
    /// Stated on the wire rather than assumed by the client, because the honest
    /// rendering depends on it: a flat transcript whose delegations are separate
    /// sessions must not be drawn as though its tool calls host trajectories. See
    /// [`normalize_events`].
    pub flat: bool,
    /// True when `limit` cut the stream short.
    pub truncated: bool,
    /// Live session id when already imported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Report a failed TraceDB call at a level that matches what it means.
///
/// A query that failed because the binary is absent is not news: the client
/// already announced that once at `error`, and it is a persistent state rather than
/// a recurring event. The sidebar polls the listing for every expanded workspace,
/// so logging it per request turns one missing install into an unbounded stream —
/// measured at 2339 identical lines in a single E2E run, which is a no-TraceDB
/// install by construction. A genuine query failure against a *reachable* archive
/// is news, and still warns.
pub(crate) fn report_tracedb_failure(state: &AppState, context: &str, detail: &str) {
    if state.tracedb.unavailability_reported() {
        tracing::debug!("{}: {}", context, detail);
    } else {
        tracing::warn!("{}: {}", context, detail);
    }
}

/// Sessions the archive names as children of `parent`, newest first.
///
/// One pass over an unfiltered `list`, matching on the id prefix. The `cwd` filter
/// is deliberately *not* used to narrow this: 484 of 2238 local delegates ran in a
/// different directory than their parent, so filtering by the parent's directory
/// would drop exactly the delegates whose location is most worth reporting — and
/// the filter is an unanchored substring match besides.
///
/// Returns `(delegates, truncated)`. An unreachable archive answers empty rather
/// than failing the transcript: the events are already in hand, and losing the
/// delegation list is not worth losing the conversation over.
async fn delegations_of(
    state: &AppState,
    parent: &str,
    parent_cwd: Option<&str>,
) -> (Vec<ArchiveDelegate>, bool) {
    let prefix = format!("{parent}/");
    let mut found: Vec<ArchiveDelegate> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut examined = 0usize;
    let parent_cwd = parent_cwd.map(normalize).unwrap_or_default();

    while examined < MAX_DELEGATION_SCAN_ROWS {
        let page = match state.tracedb.list(None, UPSTREAM_PAGE, cursor.as_deref()).await {
            Ok(page) => page,
            Err(detail) => {
                // Not an error for the caller: a transcript without its delegation
                // list is still the transcript. Logged once per request, and the
                // client is told the list is incomplete.
                report_tracedb_failure(
                    state,
                    &format!("Delegation scan failed for {}", parent),
                    &detail,
                );
                return (found, true);
            }
        };
        examined += page.sessions.len();
        let exhausted = page.next_cursor.is_none();
        cursor = page.next_cursor;

        for summary in &page.sessions {
            if !summary.id.starts_with(&prefix) {
                continue;
            }
            // An ingested-but-empty session has nothing to open.
            if summary.events == 0 {
                continue;
            }
            let cwd = summary.cwd.as_deref().map(normalize).unwrap_or_default();
            found.push(delegate_of(summary, &cwd, &parent_cwd));
        }

        if exhausted {
            return (found, false);
        }
    }

    (found, true)
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
        .find_session_by_archive_id(trace_id)
        .await?
        .map(|session| session.id);

    // Lineage from the archive's own record, with the id suffix as the fallback it
    // is elsewhere in this module: `parent_session_id` is authoritative but 199
    // subagents record only a suffix, and the reverse case exists too.
    let parent_id = match trace.session.lineage() {
        Lineage::Subagent { parent } => Some(parent.to_string()),
        _ => split_lineage(&trace.session.id).map(|(parent, _)| parent.to_string()),
    };
    let forked_from = match trace.session.lineage() {
        Lineage::Fork { origin } => Some(origin.to_string()),
        _ => None,
    };

    let (delegations, delegations_truncated) =
        delegations_of(&state, trace_id, trace.session.cwd.as_deref()).await;

    Ok(Json(TranscriptResponse {
        id: trace.session.id,
        agent: trace.session.agent,
        label,
        cwd: trace.session.cwd,
        started_at_ms: trace.session.started_at_ms,
        events,
        delegations,
        delegations_truncated,
        parent_id,
        forked_from,
        // No agent in the archive records an event-level parent pointer, so an
        // archived transcript is flat as a matter of what was stored, not of what
        // this endpoint chose to send.
        flat: true,
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
            // Always `None`, and that is a measurement rather than a shortcut.
            //
            // A live turn carries delegation in each event's `span`. Nothing in
            // the archive carries an equivalent: `data_json` — the only place a
            // raw vendor payload could survive, and where `claude`'s own
            // `parent_tool_use_id` lives — is null on all 36308 events sampled
            // across all five agents. So there is no event-level parent pointer
            // to translate.
            //
            // `parent_id` is emphatically not one, and reusing it would be the
            // damaging mistake here, because it means something different per
            // agent. Measured over 164 sessions: for `claude` it is the
            // `parentUuid` conversation chain, where 13689 of 15119 pointers
            // name the immediately preceding event and fanout is 1 — as a `span`
            // that nests a whole transcript into a single chain thousands deep.
            // `opencode` uses it to group parts under a message (fanout 4-5),
            // `pi` mixes both, and `codex` and `gemini` never set it at all.
            // Only the `tool_result` → `tool_call` case is a real relationship,
            // and that is what `stream_id` above already carries.
            //
            // Attribution to the *specific* call that delegated was measured and
            // rejected: `claude` names a child `<parent>/agent-a<name>-<hash>`,
            // but 1918 of 2104 suffixes in the local archive are hash-only with
            // no name to match, so it recovers 8.8% and silently misattributes
            // on collision. A structure that is right for one delegation in
            // eleven is worse than an honest flat transcript, because nothing on
            // screen distinguishes the two.
            //
            // So delegation is reported where the archive actually records it —
            // session level, as `subagents` on a listing row and `parentId`
            // here.
            span: None,
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

    /// Real ids from the local archive, because a wrong one passes.
    ///
    /// Both id spaces here are disjoint from Sylph's UUIDs, and an id that merely
    /// looks plausible would satisfy every assertion below while proving nothing
    /// about what TraceDB actually emits. These were read out of the archive with
    /// `trace-db api`, and the parent/child pair is a genuine `claude` delegation
    /// that ran in `/Users/hsw/Workspace/learn/ielts`.
    const REAL_PARENT: &str = "claude:53e66af0-0ffa-49ed-8026-4e3823a285a9";
    const REAL_CHILD: &str =
        "claude:53e66af0-0ffa-49ed-8026-4e3823a285a9/agent-amorphology-ch05-137ddc835b490511";
    /// A real `opencode` delegation, which records a parent while carrying no
    /// suffix — the 212 cases the id shape cannot see.
    const REAL_SUFFIXLESS_CHILD: &str = "opencode:ses_fc911ef21ffeea4YjsIy3jxkXR";
    const REAL_SUFFIXLESS_PARENT: &str = "opencode:ses_fc9401b73ffePlTal4Zxc1XuzG";

    fn trace_session(id: &str) -> crate::tracedb::TraceSession {
        crate::tracedb::TraceSession {
            id: id.to_string(),
            agent: "claude".to_string(),
            cwd: None,
            title: None,
            parent_session_id: None,
            forked_from: None,
            started_at_ms: None,
        }
    }

    #[test]
    fn a_delegated_transcript_reports_the_parent_that_spawned_it() {
        // The upward half of the session-level tree. Without it a delegate opened
        // from a search hit reads as a conversation someone started, which for half
        // the archive is wrong.
        let mut child = trace_session(REAL_CHILD);
        child.parent_session_id = Some(REAL_PARENT.to_string());

        assert_eq!(
            child.lineage(),
            Lineage::Subagent { parent: REAL_PARENT },
            "a recorded parent is the delegation link the transcript reports",
        );
    }

    #[test]
    fn a_delegate_recording_no_suffix_still_reports_its_parent() {
        // `opencode` names a child as a session of its own, so the id shape says
        // nothing. Only `parent_session_id` does, and reading the suffix alone
        // leaves this session claiming to be work someone started.
        let mut child = trace_session(REAL_SUFFIXLESS_CHILD);
        child.agent = "opencode".to_string();
        child.parent_session_id = Some(REAL_SUFFIXLESS_PARENT.to_string());

        assert_eq!(
            child.lineage(),
            Lineage::Subagent {
                parent: REAL_SUFFIXLESS_PARENT
            },
        );
        assert!(
            split_lineage(REAL_SUFFIXLESS_CHILD).is_none(),
            "this id carries no suffix, which is why the recorded parent is the only signal",
        );
    }

    #[test]
    fn an_archived_transcript_carries_no_event_level_delegation_structure() {
        // The claim the whole design rests on: the archive stores no equivalent of
        // `span`, so nothing here may synthesize one. `parent_id` is the nearest
        // field and is a different relationship — for `claude` it chains a
        // conversation, so honouring it as a span would nest a transcript into a
        // single chain thousands deep.
        let mut first = trace_event("assistant", Some("thinking about it"));
        first.native_id = Some("evt-1".to_string());
        let mut second = trace_event("assistant", Some("still thinking"));
        second.native_id = Some("evt-2".to_string());
        // The conversation chain: the second event names the first as its parent.
        second.parent_id = Some("evt-1".to_string());

        let events = normalize_events(&[first, second]);

        assert!(
            events.iter().all(|event| event.span.is_none()),
            "a conversation chain must never be published as delegation structure",
        );
    }

    #[test]
    fn a_result_still_pairs_with_its_call_even_though_nothing_gets_a_span() {
        // The one real relationship `parent_id` does carry. Dropping spans must not
        // cost this, or an archived tool call loses its output.
        let mut call = trace_event("tool_call", Some("{}"));
        call.native_id = Some("call-evt".to_string());
        let mut result = trace_event("tool_result", Some("output"));
        result.parent_id = Some("call-evt".to_string());

        let events = normalize_events(&[call, result]);

        assert_eq!(events[0].stream_id, events[1].stream_id);
        assert!(events.iter().all(|event| event.span.is_none()));
    }

    #[test]
    fn a_delegate_row_reports_its_directory_only_when_it_differs_from_its_parent() {
        // 484 of 2238 subagents ran somewhere else, typically a worktree, and that
        // difference is worth showing. Equality is the common case and repeating the
        // parent's own path on every child row is noise.
        let summary = crate::tracedb::SessionSummary {
            id: REAL_CHILD.to_string(),
            agent: "claude".to_string(),
            cwd: Some("/Users/hsw/Workspace/learn/ielts".to_string()),
            title: None,
            model: None,
            started_at_ms: None,
            ended_at_ms: None,
            events: 14,
        };

        let same = delegate_of(
            &summary,
            "/Users/hsw/Workspace/learn/ielts",
            "/Users/hsw/Workspace/learn/ielts",
        );
        assert_eq!(same.cwd, None, "a same-directory delegate repeats no path");
        assert_eq!(same.id, REAL_CHILD);
        assert_eq!(same.events, 14);

        let elsewhere = delegate_of(
            &summary,
            "/Users/hsw/Workspace/learn/ielts/worktree",
            "/Users/hsw/Workspace/learn/ielts",
        );
        assert_eq!(
            elsewhere.cwd.as_deref(),
            Some("/Users/hsw/Workspace/learn/ielts/worktree"),
            "a delegate that ran elsewhere must say so",
        );
    }

    #[tokio::test]
    async fn an_absent_archive_is_reported_once_rather_than_once_per_request() {
        // A supported degraded configuration must not produce a log line per
        // request. The client announces the absent binary once at `error`; a caller
        // that warns anyway rebuilds the unbounded stream one layer up, which is
        // what the sidebar did to this endpoint — 2339 identical lines in one E2E
        // run against a no-TraceDB install.
        let client = crate::tracedb::Client::new("/nonexistent/trace-db".into());

        assert!(
            !client.unavailability_reported(),
            "nothing has been said before the first attempt",
        );
        assert!(!client.probe().await, "an absent binary cannot answer");
        assert!(
            client.unavailability_reported(),
            "after the first failure a caller must be able to see it was announced",
        );
        // Still unavailable, and still known to have been announced: quieter
        // logging must never make an absent archive look present.
        assert!(!client.probe().await);
        assert!(client.unavailability_reported());
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
