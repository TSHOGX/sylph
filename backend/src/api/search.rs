//! Search the durable TraceDB archive.
//!
//! This reads archived coding-agent history through TraceDB's own JSON API, per
//! the project invariant that TraceDB's SQLite schema is never queried
//! directly. TraceDB owns lineage collapsing and relevance ranking; the backend
//! only annotates hits with whether a live session already mirrors them.

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::tracedb::SearchHit;
use crate::AppState;

#[derive(Deserialize)]
pub struct SearchQuery {
    /// Free-text query. Whitespace-only is treated as absent.
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultItem {
    #[serde(flatten)]
    hit: SearchHit,
    /// Live session id when this archived session has already been imported,
    /// which lets the UI offer "open" instead of a duplicate "import".
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    results: Vec<SearchResultItem>,
    /// False when TraceDB could not be reached at all.
    available: bool,
    /// Why the archive is unavailable, for display. Absent when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// GET /api/v1/search?q={query}&limit={n}
///
/// TraceDB being absent is a normal local state, not a server fault: the
/// backend boots without it and the rest of the app works. So an unreachable
/// archive answers 200 with `available: false` rather than 503, which lets the
/// UI show "archive unavailable" without treating it as an error to retry.
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<SearchResponse>> {
    let query = params.q.trim();
    if query.is_empty() {
        return Ok(Json(SearchResponse {
            results: Vec::new(),
            available: true,
            detail: None,
        }));
    }

    let limit = params.limit.clamp(1, 100);

    let hits = match state.tracedb.search(query, limit).await {
        Ok(hits) => hits,
        Err(detail) => {
            // One report per run, not one per keystroke — the shared helper, so this
            // endpoint cannot drift from `api::archive` and `api::workspaces`. An
            // unreachable archive is one fact about the install, and a `warn` per
            // keystroke-driven search would be an unbounded stream describing a
            // supported configuration. Unconditional `debug` was the alternative and
            // hides the first failure too, which is the one worth seeing. The
            // response carries `available: false` and `detail` regardless, which is
            // what the UI renders.
            crate::api::archive::report_tracedb_failure(
                &state,
                "TraceDB search failed",
                &detail,
            );
            return Ok(Json(SearchResponse {
                results: Vec::new(),
                available: false,
                detail: Some(detail),
            }));
        }
    };

    let mut results = Vec::with_capacity(hits.len());
    for hit in hits {
        // Sequential rather than concurrent: the lookup is an indexed
        // single-row query and `limit` is capped at 100, so the round trips
        // cost less than fanning out over the connection pool.
        let session_id = state
            .store
            .find_session_by_archive_id(&hit.id)
            .await?
            .map(|session| session.id);
        results.push(SearchResultItem { hit, session_id });
    }

    Ok(Json(SearchResponse {
        results,
        available: true,
        detail: None,
    }))
}
