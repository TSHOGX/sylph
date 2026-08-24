use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::{
    db::store::SessionFilter,
    error::{Error, Result},
    events::StreamEvent,
    models::{CreateSessionRequest, Session, Turn},
    tracedb::summarize_line,
    AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    agent: Option<String>,
    origin: Option<String>,
    workdir: Option<String>,
    updated_after_ms: Option<i64>,
    updated_before_ms: Option<i64>,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct SessionListResponse {
    sessions: Vec<Session>,
}

/// Blank text in a query string is the natural encoding of a cleared filter
/// input, so it means "unfiltered" rather than "match the empty string".
fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<SessionListResponse>> {
    let origin = non_blank(query.origin);
    if let Some(origin) = &origin {
        // Origin is a closed set that drives identity and lifecycle policy;
        // a typo must not silently return an empty list.
        if origin != "sylph" && origin != "native" {
            return Err(Error::InvalidRequest(format!(
                "Unknown origin: {origin}. Expected \"sylph\" or \"native\"."
            )));
        }
    }

    let filter = SessionFilter {
        limit: query.limit.clamp(1, 200),
        agent: non_blank(query.agent),
        origin,
        workdir: non_blank(query.workdir),
        updated_after_ms: query.updated_after_ms,
        updated_before_ms: query.updated_before_ms,
    };

    let sessions = state.store.list_sessions(&filter).await?;
    Ok(Json(SessionListResponse { sessions }))
}

#[derive(Serialize)]
pub struct SessionResponse {
    session: Session,
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionResponse>> {
    let session = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    Ok(Json(SessionResponse { session }))
}

/// Discard an untouched Sylph draft. Native sessions and sessions with turns
/// are durable history and cannot be removed through this endpoint.
pub async fn delete_empty(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.store.delete_empty_sylph_session(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// One turn together with its durable events, oldest event first.
#[derive(Serialize)]
pub struct TurnWithEvents {
    turn: Turn,
    events: Vec<StreamEvent>,
}

#[derive(Serialize)]
pub struct HistoryResponse {
    turns: Vec<TurnWithEvents>,
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_history_limit")]
    limit: i64,
}

fn default_history_limit() -> i64 {
    100
}

/// GET /api/v1/sessions/{id}/turns — durable conversation history.
///
/// The SSE endpoint covers a single turn, which is enough to watch work happen
/// but loses everything on reload. This returns the whole session so the
/// conversation survives a refresh.
///
/// Only journal rows are read, never the ephemeral projection: `delta` events
/// are transient by protocol and their text is already contained in the
/// matching `completed` event, so replaying both would duplicate every token.
pub async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>> {
    // 404 rather than an empty list, so a mistyped id is distinguishable from
    // a session that genuinely has no turns yet.
    state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    let limit = query.limit.clamp(1, 500);

    // `list_turns` orders newest-first for the session list; a transcript reads
    // oldest-first.
    let mut turns = state.store.list_turns(&id, limit).await?;
    turns.reverse();

    let mut out = Vec::with_capacity(turns.len());
    for turn in turns {
        // Both tiers, because a turn's events live in exactly one of them and
        // which one depends on whether consolidation has run. Reading only the
        // journal would make history vanish from the UI moments after a turn
        // finished, which is the bug this exists to prevent.
        let journalled = state.store.turn_events_after(&turn.id, -1).await?;
        let events = if journalled.is_empty() {
            state
                .history
                .turn_blocks(&turn.id)
                .await
                .map(|blocks| blocks.iter().map(block_to_event).collect())
                .unwrap_or_default()
        } else {
            journalled.iter().map(StreamEvent::from_row).collect()
        };
        out.push(TurnWithEvents { turn, events });
    }

    Ok(Json(HistoryResponse { turns: out }))
}

/// Render a consolidated block as the event a client expects.
///
/// A timeline renders stable events, and a block *is* a stable event plus how it
/// streamed — so the folded deltas are deliberately not expanded here. Replay is a
/// separate operation for a client that wants the arrival sequence; a transcript
/// read wants the settled value.
pub fn block_to_event(block: &crate::storage::Block) -> StreamEvent {
    StreamEvent {
        seq: block.seq,
        kind: block.kind.clone(),
        phase: block.phase.clone(),
        stream_id: block.stream_id.clone(),
        name: block.name.clone(),
        text: block.text.clone(),
        data: block.data.clone(),
        is_error: block.is_error,
        stability: "stable".to_string(),
        source: block.source.clone(),
        created_at_ms: block.created_at_ms,
    }
}

pub async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>)> {
    // Validate agent exists
    let registry = state.registry.lock().await;
    if !registry.has_agent(&req.agent) {
        return Err(Error::InvalidRequest(format!(
            "Unknown agent: {}. Available agents: {:?}",
            req.agent,
            registry
                .list_agents()
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
        )));
    }
    drop(registry);

    // Validate workdir exists
    if !std::path::Path::new(&req.workdir).exists() {
        return Err(Error::InvalidRequest(
            "Workdir does not exist".to_string(),
        ));
    }

    // Origin stays `sylph`: this session was created by the WebApp, and origin
    // records who created it. Identity is a separate axis, and opting out of
    // injection does not make the session native — Sylph lifecycle policy still
    // applies, and its history was not produced elsewhere. Collapsing the two
    // would make "run this agent as itself" indistinguishable from an imported
    // session, which must never be reset or compacted.
    let identity_mode = req.identity_mode.unwrap_or_default();

    let now = chrono::Utc::now().timestamp_millis();
    let session = Session {
        id: uuid::Uuid::new_v4().to_string(),
        origin: "sylph".to_string(),
        identity_mode: identity_mode.as_str().to_string(),
        lifecycle_mode: "sylph".to_string(),
        agent: req.agent.clone(),
        backend_session_id: None,
        trace_session_id: None,
        workdir: req.workdir.clone(),
        title: req.title.clone(),
        flags_json: req
            .flags
            .map(|f| serde_json::to_string(&f).unwrap())
            .unwrap_or_else(|| "{}".to_string()),
        status: "idle".to_string(),
        created_at_ms: now,
        updated_at_ms: now,
    };

    let created = state.store.create_session(&session).await?;
    Ok((StatusCode::CREATED, Json(SessionResponse { session: created })))
}


#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSessionRequest {
    /// Complete replacement flag set. Absent leaves flags untouched; an empty
    /// map clears them.
    pub flags: Option<std::collections::HashMap<String, String>>,
    /// Display title. Absent leaves it untouched.
    pub title: Option<String>,
}

/// PATCH /api/v1/sessions/{id}
///
/// Change runner flags (model, reasoning effort) or the title of an existing
/// session.
///
/// # Why flags are validated here rather than at turn time
///
/// `POST /sessions/{id}/turns` filters the stored flag set against the runner's
/// declared schema and silently drops what it does not recognize. That is the
/// right behaviour there — a descriptor can change under a session that was
/// created before it, and a turn should still run. It is the wrong behaviour for
/// a user action: a model chosen in the composer that the runner never declared
/// would be accepted, stored, dropped at turn time, and the turn would run on
/// the default with nothing anywhere saying why. Validating on write turns that
/// silent no-op into an error the UI can show against the control that caused it.
///
/// Identity is deliberately not patchable. `agent`, `origin`, `identityMode`, and
/// `lifecycleMode` decide which runner owns the conversation and whether `SOUL.md`
/// may be injected; changing them on a session with history would rewrite what
/// that history was produced by. Native-to-Sylph conversion is a separate,
/// explicit operation for exactly this reason.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSessionRequest>,
) -> Result<Json<SessionResponse>> {
    let session = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    if let Some(flags) = &req.flags {
        let registry = state.registry.lock().await;
        let descriptor = registry
            .list_agents()
            .into_iter()
            .find(|agent| agent.name == session.agent);
        drop(registry);

        // A session whose runner has been uninstalled cannot have its flags
        // validated, so it cannot have them changed. Failing is better than
        // storing an unvalidated value that would be dropped at turn time.
        let descriptor = descriptor.ok_or_else(|| {
            Error::InvalidRequest(format!(
                "Agent '{}' is not currently installed, so its flags cannot be changed.",
                session.agent
            ))
        })?;

        for (key, value) in flags {
            let spec = descriptor.flags.get(key).ok_or_else(|| {
                Error::InvalidRequest(format!(
                    "Agent '{}' does not declare a '{key}' flag.",
                    session.agent
                ))
            })?;
            // An empty `choices` list means the flag is free-form, so only
            // enumerated flags are checked against their enumeration.
            if !spec.choices.is_empty() && !spec.choices.contains(value) {
                return Err(Error::InvalidRequest(format!(
                    "'{value}' is not a valid {key} for '{}'. Valid choices: {:?}",
                    session.agent, spec.choices
                )));
            }
        }

        let flags_json = serde_json::to_string(flags)
            .map_err(|e| Error::Internal(format!("Failed to encode flags: {e}")))?;
        state.store.set_session_flags(&id, &flags_json).await?;
    }

    if let Some(title) = &req.title {
        state.store.set_session_title(&id, title.trim()).await?;
    }

    let updated = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    Ok(Json(SessionResponse { session: updated }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSessionRequest {
    /// TraceDB session id, e.g. `claude:uuid`.
    pub trace_session_id: String,
    /// Optional override; otherwise derived from the archived session's
    /// opening user turn.
    pub title: Option<String>,
}

/// Split `claude:uuid` into its agent prefix and native id.
///
/// The agent is taken from the id rather than from the request body because it
/// selects identity and lifecycle policy. A client that named the wrong agent
/// would produce a session governed by the wrong rules, so the value has to
/// come from the archive's own naming.
fn split_trace_id(trace_id: &str) -> Option<(&str, &str)> {
    let (agent, native) = trace_id.split_once(':')?;
    if agent.is_empty() || native.is_empty() {
        return None;
    }
    Some((agent, native))
}

/// POST /api/v1/sessions/import — adopt an archived native session.
///
/// A native session is restored as it was recorded. Its originating agent's
/// identity and lifecycle are preserved: no SOUL.md injection, no reset, no
/// compaction, no prompt rewriting. That is why `identityMode` and
/// `lifecycleMode` are `preserve` here rather than `sylph`, and why every
/// policy-relevant field is derived from the archive instead of the request.
pub async fn import(
    State(state): State<AppState>,
    Json(req): Json<ImportSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>)> {
    let trace_id = req.trace_session_id.trim();
    let (agent, _native_id) = split_trace_id(trace_id).ok_or_else(|| {
        Error::InvalidRequest(
            "traceSessionId must be formatted as \"<agent>:<id>\", as TraceDB reports it."
                .to_string(),
        )
    })?;

    // Importing twice would produce two live sessions mirroring one archived
    // session, and each would resume the same native history independently.
    if state.store.find_session_by_archive_id(trace_id).await?.is_some() {
        return Err(Error::SessionAlreadyImported);
    }

    // Resuming needs a runner for the originating agent. Without one the
    // session could be listed but never continued, so refuse up front rather
    // than create something inert.
    {
        let registry = state.registry.lock().await;
        if !registry.has_agent(agent) {
            return Err(Error::InvalidRequest(format!(
                "No runner for agent {agent:?}, which this archived session was created by. \
                 A native session can only be imported when its own agent is available, \
                 because its identity and lifecycle must be preserved."
            )));
        }
    }

    let facts = state
        .tracedb
        .session_facts(trace_id)
        .await
        .map_err(Error::TraceDbUnavailable)?
        .ok_or_else(|| Error::TraceSessionNotFound(trace_id.to_string()))?;

    // Without a recorded working directory there is nothing to resume into.
    let workdir = facts
        .cwd
        .ok_or_else(|| Error::TraceSessionNotResumable(trace_id.to_string()))?;
    if !std::path::Path::new(&workdir).exists() {
        return Err(Error::InvalidRequest(format!(
            "The working directory this session ran in no longer exists: {workdir}"
        )));
    }

    let title = req
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| facts.first_ask.as_deref().map(summarize_ask));

    let now = chrono::Utc::now().timestamp_millis();
    let session = Session {
        id: uuid::Uuid::new_v4().to_string(),
        origin: "native".to_string(),
        // `preserve`, not `native`: the mode names the policy, which is to keep
        // whatever the originating agent established.
        identity_mode: "preserve".to_string(),
        lifecycle_mode: "preserve".to_string(),
        agent: agent.to_string(),
        backend_session_id: None,
        trace_session_id: Some(trace_id.to_string()),
        workdir,
        title,
        flags_json: "{}".to_string(),
        status: "idle".to_string(),
        // Keep the archived session's own start time so it sorts by when the
        // work actually happened, not when it was adopted.
        created_at_ms: facts.started_at_ms.unwrap_or(now),
        updated_at_ms: now,
    };

    let created = state.store.create_session(&session).await?;
    Ok((
        StatusCode::CREATED,
        Json(SessionResponse { session: created }),
    ))
}

/// First meaningful line of the opening ask, capped, for use as a title.
///
/// Shares one implementation with the archive browser's row labels: an imported
/// session and its archived counterpart must not be named differently, or the
/// same conversation reads as two.
fn summarize_ask(ask: &str) -> String {
    summarize_line(ask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_imported_title_is_the_ask_not_the_injected_preamble() {
        // Before this filter, every imported codex session was titled with the
        // same sandboxing boilerplate.
        let ask = "<permissions instructions> Filesystem sandboxing defines which files\n\
                   Fix the SSE reconnect loop";

        assert_eq!(summarize_ask(ask), "Fix the SSE reconnect loop");
    }

    #[test]
    fn a_plain_ask_is_unchanged() {
        assert_eq!(summarize_ask("Fix the login bug"), "Fix the login bug");
        // A subagent brief is a real ask, not boilerplate.
        assert_eq!(
            summarize_ask("You are reviewing Task 4."),
            "You are reviewing Task 4."
        );
    }

    #[test]
    fn a_long_ask_is_truncated_on_a_character_boundary() {
        // Non-ASCII is the norm in this archive; slicing by byte would panic.
        let ask = "请帮我".repeat(40);
        let summary = summarize_ask(&ask);

        assert!(summary.ends_with('…'));
        assert_eq!(summary.chars().count(), 81, "80 chars plus the ellipsis");
    }

    #[test]
    fn an_envelope_only_ask_still_yields_a_title() {
        // Slash-command sessions carry nothing else. An empty title would render
        // as a blank row, which is worse than showing the envelope.
        assert_eq!(
            summarize_ask("<command-name>/theme</command-name>"),
            "<command-name>/theme</command-name>"
        );
    }
}
