//! Cross-session full-text search over consolidated history.
//!
//! # Search is the one capability this rewrite takes from working to absent
//!
//! `/api/v1/search` was entirely `state.tracedb.search()`. Deleting the separate archive
//! deleted the only implementation, and nothing in this repo has ever held an FTS index —
//! so this is net-new work rather than a port (`docs/database-schema.md` § Search lives
//! beside `history.db`).
//!
//! **It now covers everything the server holds, and that is what the rewrite bought.**
//! Search used to run against a store fed by parsing native files, so it covered only
//! what a parser had read: a session whose environment had gone away was searchable
//! nowhere, and one whose vendor file happened to be local was searchable by coincidence.
//! The index is over the same rows the timeline renders, so the old caveat — "a client
//! must not present this as *search my history*" — is retired rather than relocated.

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use sylph_protocol::EnvironmentId;

use crate::error::{Error, Result};
use crate::models::Session;
use crate::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    /// Free-text query. Whitespace-only is treated as absent.
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
    /// Narrow to one environment.
    environment: Option<String>,
}

fn default_limit() -> usize {
    20
}

/// Where in a session the query matched.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BestMatch {
    /// The sequence number a client scrolls to.
    pub seq: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// The only text on the row containing the query.
    pub excerpt: String,
}

/// One matching session.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    /// **The same object the listing endpoints return**, so a client renders one row
    /// component for both and never has a shape that exists only in search.
    pub session: Session,
    /// Omitted rather than zeroed when the index cannot supply it. A client must not read
    /// an absent `bestMatch` as "matched at the beginning".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_match: Option<BestMatch>,
    /// How many blocks matched within this session. Omitted, never zeroed, for the same
    /// reason as `bestMatch`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hits: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    results: Vec<SearchResult>,
}

/// GET /api/v1/search?q={query}&limit={n}&environment={id}
///
/// # There is no `available` flag any more, and its absence is the point
///
/// The old response carried `available: false` plus a `detail`, because the archive was a
/// separate binary that might not be installed — a supported degraded state the UI had to
/// render. One store means there is no such state: the index is beside `history.db`, which
/// the server opened at startup or failed to boot. A flag that can only ever be `true` is
/// a lever that changes nothing, and `AGENTS.md` records why offering one is worse than
/// not.
///
/// # Never 503, and empty is a real answer
///
/// Whitespace-only `q` is treated as absent and answers empty **without touching the
/// index**, so a client may bind the field directly to keystrokes.
///
/// # This read stays off any polled path
///
/// Measured, FTS5 search is 174–260 ms — and 273 ms under concurrent write load — while
/// every other read in this store is microseconds to sub-millisecond. FTS5 work is real
/// work and owning the store makes it no cheaper, so the cost model that governs every
/// other endpoint does not transfer here. It is user-initiated by design.
///
/// # The CJK limitation is stated rather than papered over
///
/// The tokenizer is `unicode61`, which splits on non-alphanumerics — so a run of Han
/// characters becomes **one token** and a query for a substring of it does not match. That
/// matters here, because this project's own transcripts are substantially Chinese.
/// `docs/storage.md` § "`blocks_fts` is standalone" carries the reasoning and names the
/// reference implementation (a loadable jieba tokenizer) for when segmented CJK search is
/// wanted; it is deliberately not adopted yet, because a loadable SQLite extension is a
/// per-platform build artifact taken on for a capability that is currently zero.
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<SearchResponse>> {
    let query = params.q.trim();
    if query.is_empty() {
        return Ok(Json(SearchResponse {
            results: Vec::new(),
        }));
    }

    // Validated rather than passed through: an environment id is base32 by construction,
    // so a malformed one can only ever match nothing — and an empty result set is
    // indistinguishable from a correct answer.
    let environment = match params.environment.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(
            EnvironmentId::parse(raw).map_err(|error| Error::InvalidRequest(error.to_string()))?,
        ),
        _ => None,
    };

    let hits = state
        .history
        .search(query, params.limit.clamp(1, 100))
        .await
        .map_err(|error| Error::Internal(format!("Search failed: {error:#}")))?;

    let mut results = Vec::with_capacity(hits.len());
    for hit in hits {
        // A hit names a session by its encoded triple. One that will not decode is a
        // corrupt index entry rather than a bad request, and it is **skipped** rather
        // than surfaced: a result a client cannot navigate to is worse than one row
        // fewer, and there is nothing a user could do about it.
        let Ok(session_id) = sylph_protocol::SessionId::decode(&hit.session_id) else {
            tracing::warn!("Search index holds an unparseable session id: {}", hit.session_id);
            continue;
        };
        if environment
            .as_ref()
            .is_some_and(|wanted| wanted != &session_id.environment)
        {
            continue;
        }
        // Skipped when the journal no longer has the row. History outliving its session
        // row is possible, and a result carrying no session is a shape the response
        // deliberately cannot express — `session` is not optional.
        let Some(session) = state.store.get_session(&session_id).await? else {
            continue;
        };

        results.push(SearchResult {
            session,
            best_match: Some(BestMatch {
                seq: hit.seq,
                turn_id: hit.turn_id,
                excerpt: hit.excerpt,
            }),
            hits: Some(hit.hits),
        });
    }

    Ok(Json(SearchResponse { results }))
}
