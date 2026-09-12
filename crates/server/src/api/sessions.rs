//! Sessions: listing, reading, starting, adopting, and patching.

use crate::models::now_ms;
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sylph_protocol::{AgentName, EnvironmentId, NativeId, SessionId};

use crate::middleware::auth::SubjectId;
use crate::{
    db::store::SessionFilter,
    env::bridge::RemoteCommandError,
    error::{Error, Result},
    events::StreamEvent,
    models::{AdoptSessionRequest, CreateSessionRequest, Session, Turn},
    AppState,
};

/// Decode a session id out of a path segment, or refuse it.
///
/// # Why every handler goes through this
///
/// A path segment is the encoded triple (`docs/api/README.md` § A session id in a
/// path), and a malformed one is a **400 rather than a 404**: the request is
/// unintelligible, not pointing at something absent. Collapsing the two would tell a
/// client with a typo'd id that the session does not exist, which sends the
/// investigation to the wrong layer — and a client that had corrupted an id would see
/// the same answer as one asking about deleted work.
///
/// It is a function rather than an extractor because the failure has to carry the
/// decoder's own reason (`sylph_protocol::IdentityError` names which of the three
/// parts is wrong), and an extractor's rejection cannot reach [`Error`] without
/// flattening that.
pub fn decode_id(encoded: &str) -> Result<SessionId> {
    SessionId::decode(encoded).map_err(|error| Error::InvalidRequest(error.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    agent: Option<String>,
    environment: Option<String>,
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

/// Blank text in a query string is the natural encoding of a cleared filter input, so
/// it means "unfiltered" rather than "match the empty string".
fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// GET /api/v1/sessions
///
/// **One list, one store.** There is no live/archived split and no deduplication step,
/// because native history is parsed into the same events a live turn emits and lands
/// in the same store (`docs/architecture.md` § One authority).
pub async fn list(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Query(query): Query<ListQuery>,
) -> Result<Json<SessionListResponse>> {
    // Validated rather than passed through: an environment id is base32 by
    // construction, so a malformed one can only ever match nothing — and an empty
    // list is indistinguishable from a correct answer. The `origin` filter this
    // replaced was validated for the same reason.
    let environment = match non_blank(query.environment) {
        Some(raw) => Some(
            EnvironmentId::parse(raw)
                .map_err(|error| Error::InvalidRequest(error.to_string()))?
                .to_string(),
        ),
        None => None,
    };

    if let Some(ref env) = environment {
        if !state
            .store
            .has_capability(&subject.0, env, "observe")
            .await?
        {
            return Ok(Json(SessionListResponse { sessions: vec![] }));
        }
    }
    let filter = SessionFilter {
        limit: query.limit.clamp(1, 200),
        agent: non_blank(query.agent),
        environment,
        // Normalized here so the exact match in SQL compares the same spelling the
        // workspace grouping uses.
        workdir: non_blank(query.workdir).map(|path| crate::api::workspaces::normalize(&path)),
        updated_after_ms: query.updated_after_ms,
        updated_before_ms: query.updated_before_ms,
    };

    let sessions = state.store.list_sessions(&filter).await?;
    Ok(Json(SessionListResponse { sessions }))
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub session: Session,
}

#[derive(Serialize)]
pub struct SessionEdgesResponse {
    pub edges: Vec<sylph_protocol::SessionEdge>,
}

/// GET /api/v1/sessions/{id}/edges
pub async fn edges(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionEdgesResponse>> {
    let id = decode_id(&id)?;
    state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    Ok(Json(SessionEdgesResponse {
        edges: state.store.list_session_edges(Some(&id)).await?,
    }))
}

/// Journal the typed ancestry edge an adopted session's vendor recorded.
///
/// # Why this is not a column on the session row
///
/// It was, and `sessions.parent_session` is gone because a string cannot carry relation
/// type, provenance, and more than one edge at once (ADR-0001 § 2). The column was also
/// never read: it was written by both adopt paths, serialized as `parentSession`, and no
/// reader anywhere consumed it — a field that only ever wrote is indistinguishable from a
/// working one, which is why it survived.
///
/// # Nothing is written when the relation is unknown
///
/// `parent_relation` is `None` where a vendor names a parent without saying how, and this
/// writes no edge for that case. A guessed `delegation` would render a fork as a folded
/// count and a guessed `fork` would replay a prefix that was never shared, and neither is
/// withdrawable once journaled — a later fix to this function cannot tell a guessed row
/// from a real one. An unrecorded relation is the honest flat view.
///
/// # Failure is logged, not returned
///
/// The child row is already committed and the adopt succeeded. Ancestry is a read model
/// (`session_edges` is documented as one), re-derivable by re-adopting, so failing the
/// request here would refuse a session that exists over a decoration that does not.
async fn record_adopted_lineage(
    state: &AppState,
    child: &sylph_protocol::SessionId,
    parent_native_id: Option<&NativeId>,
    relation: Option<sylph_protocol::SessionEdgeRelation>,
) {
    let (Some(parent_native_id), Some(relation)) = (parent_native_id, relation) else {
        return;
    };
    // A vendor's parent id is on the same machine and from the same vendor by
    // construction: it is a pointer within one native store.
    let parent = sylph_protocol::SessionId::new(
        child.environment.clone(),
        child.agent.clone(),
        parent_native_id.clone(),
    );
    // A store that names a session as its own parent is malformed, not a self-edge to
    // journal. `SessionEdge::new` refuses it, and that refusal is the whole check.
    let edge = match sylph_protocol::SessionEdge::new(parent, child.clone(), relation) {
        Ok(edge) => edge,
        Err(error) => {
            tracing::warn!("Ignoring {child}'s recorded ancestry: {error}");
            return;
        }
    };
    if let Err(error) = state.store.insert_session_edge(&edge).await {
        tracing::warn!("Could not journal {child}'s {relation} edge: {error:#}");
    }
}

#[derive(Serialize)]
pub struct PreparedSessionResponse {
    /// Opaque, process-local handle. It is intentionally not a SessionId and is not
    /// accepted by history/listing endpoints.
    #[serde(rename = "draftId")]
    pub draft_id: String,
    pub environment: EnvironmentId,
    pub agent: String,
    pub workdir: String,
    pub title: Option<String>,
    #[serde(rename = "injectIdentity")]
    pub inject_identity: bool,
}

pub async fn get(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<SessionResponse>> {
    let id = decode_id(&id)?;
    let session = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    if !state
        .store
        .has_capability(&subject.0, session.id.environment.as_str(), "observe")
        .await?
    {
        return Err(Error::SessionNotFound);
    }
    Ok(Json(SessionResponse { session }))
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
/// The SSE endpoint covers a single turn, which is enough to watch work happen but
/// loses everything on reload. This returns the whole session so the conversation
/// survives a refresh.
///
/// Only journal rows are read, never the ephemeral projection: `delta` events are
/// transient by protocol and their text is already contained in the matching
/// `completed` event, so replaying both would duplicate every token.
pub async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>> {
    let id = decode_id(&id)?;
    // 404 rather than an empty list, so a mistyped id is distinguishable from a
    // session that genuinely has no turns yet.
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
        // Both tiers, because a turn's events live in exactly one of them and which
        // one depends on whether consolidation has run. Reading only the journal would
        // make history vanish from the UI moments after a turn finished, which is the
        // bug this exists to prevent.
        //
        // **Both tiers project through `consolidate`, so there is one derivation of a
        // transcript rather than one per tier.** Reading the journal's stable rows
        // directly is the obvious shape and is wrong: a `Block` carries fields an
        // `Event` row has no column for — `started_at_ms` above all — so a turn read
        // before its sweep ran was missing the anchor every duration is measured from,
        // and acquired it seconds later when the tier changed underneath the reader.
        // Nothing failed; the number simply appeared. That is why the shared projection
        // is the fix and not a second mapping kept in step by hand: the two agreed
        // whenever a test happened to read after consolidation, which is exactly when
        // nobody was looking at the disagreement.
        let journalled = state.store.turn_events_for_consolidation(&turn.id).await?;
        let events: Vec<StreamEvent> = if journalled.is_empty() {
            state
                .history
                .turn_blocks(&turn.id)
                .await
                .map(|blocks| blocks.iter().map(block_to_event).collect())
                .unwrap_or_default()
        } else {
            crate::storage::history::consolidate(&journalled)
                .iter()
                .map(block_to_event)
                .collect()
        };
        out.push(TurnWithEvents { turn, events });
    }

    Ok(Json(HistoryResponse { turns: out }))
}

/// Render a consolidated block as the event a client expects.
///
/// A timeline renders stable events, and a block *is* a stable event plus how it
/// streamed — so the folded deltas are deliberately not expanded here. Replay is a
/// separate operation for a client that wants the arrival sequence; a transcript read
/// wants the settled value.
pub fn block_to_event(block: &crate::storage::Block) -> StreamEvent {
    StreamEvent {
        seq: block.seq,
        arrival_turn_id: Some(block.turn_id.clone()),
        kind: block.kind.clone(),
        phase: block.phase.clone(),
        stream_id: block.stream_id.clone(),
        span: block.span.clone(),
        interior_state: block
            .interior_state
            .as_deref()
            .and_then(parse_interior_state),
        name: block.name.clone(),
        text: block.text.clone(),
        data: block.data.clone(),
        is_error: block.is_error,
        stability: "stable".to_string(),
        source: block.source.clone(),
        arrival_at_ms: block.arrival_at_ms,
        recorded_at_ms: block.recorded_at_ms,
        created_at_ms: block.created_at_ms,
        // The block already carries this; it was simply never published. Without it a
        // settled block reports only when it closed, so every duration computed from
        // durable history was a reconstruction from whatever the interior happened to
        // forward — exact for a chatty delegate, a lower bound for a quiet one, and
        // absent entirely for one that forwarded nothing.
        // The anchor is retained for both clock domains. The renderer chooses recorded
        // start/end when `recordedAtMs` exists, otherwise arrival start/end for a stream
        // record; block/message fidelity gates native arrival stamps before arithmetic.
        started_at_ms: Some(block.started_at_ms),
    }
}

fn parse_interior_state(value: &str) -> Option<sylph_protocol::InteriorState> {
    sylph_protocol::InteriorState::parse(value)
}

/// POST /api/v1/sessions
///
/// **This starts an agent and returns an in-memory draft.** It does not reserve a row.
///
/// A session has no identity until an agent has assigned one
/// (`docs/architecture.md` § Identity), so the draft is kept only by the link manager.
/// The first real turn atomically writes the session and turn; discarding the draft
/// closes the process without leaving any history behind.
pub async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<PreparedSessionResponse>)> {
    // All new sessions use the same prepared lifecycle. There is no eager-create
    // compatibility path: a session becomes durable only after its first real turn.
    create_prepared(State(state), Json(req)).await
}

/// Start an agent without durable history for POST /api/v1/sessions.
///
/// This endpoint is the draft half of the two-phase lifecycle.  It deliberately
/// returns only an opaque handle; no `sessions` row is written until the first turn
/// promotes the prepared link.
async fn create_prepared(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<PreparedSessionResponse>)> {
    let environment = require_environment(&state, &req.environment).await?;
    // Every environment is an envagent. The server never starts an adapter in-process;
    // even the machine hosting this HTTP process must be paired separately. This check is
    // deliberately before any adapter or filesystem access so a missing envagent cannot
    // fall through to a local process with a foreign or guessed identity.
    if !state.env_bridge.environment_connected(&environment).await {
        return Err(Error::EnvironmentUnreachable(environment.to_string()));
    }

    let flags = req
        .flags
        .as_ref()
        .and_then(|value| value.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(key, value)| value.as_str().map(|text| (key, text.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let identity_preamble = req
        .inject_identity
        .then(|| {
            state
                .config
                .soul_path
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
        })
        .flatten();
    let (draft_id, workdir) = state
        .env_bridge
        .prepare_remote(
            &environment,
            req.agent.clone(),
            req.workdir.clone(),
            title.clone(),
            req.inject_identity,
            flags,
            identity_preamble,
            None,
        )
        .await
        .map_err(|error| {
            if let Some(remote) = error.downcast_ref::<RemoteCommandError>() {
                if let Some(code) = remote.code() {
                    return match sylph_protocol::RejectCode::parse(code) {
                        Some(sylph_protocol::RejectCode::AgentUnknown) => {
                            Error::InvalidRequest(format!("Unknown agent: {}", req.agent))
                        }
                        Some(sylph_protocol::RejectCode::AgentUnavailable) => {
                            Error::InvalidRequest(format!("Agent is not installed: {}", req.agent))
                        }
                        Some(
                            sylph_protocol::RejectCode::WorkdirOutsideRoots
                            | sylph_protocol::RejectCode::PolicyViolation,
                        ) => Error::Forbidden(
                            format!("The requested working directory is outside the configured sessions may run in roots: {}", state.config.browse_roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")),
                        ),
                        Some(sylph_protocol::RejectCode::WorkdirMissing) => {
                            Error::InvalidRequest("Working directory does not exist".into())
                        }
                        Some(sylph_protocol::RejectCode::WorkdirNotADirectory) => {
                            Error::InvalidRequest("Working directory is not a directory".into())
                        }
                        Some(sylph_protocol::RejectCode::AgentStartFailed) => {
                            Error::HandshakeFailed(remote.message().to_string())
                        }
                        _ => Error::AgentStartFailed(anyhow::anyhow!(remote.message().to_string())),
                    };
                }
                return Error::AgentStartFailed(anyhow::anyhow!(remote.message().to_string()));
            }
            Error::AgentStartFailed(error)
        })?;
    Ok((
        StatusCode::CREATED,
        Json(PreparedSessionResponse {
            draft_id,
            environment,
            agent: req.agent,
            workdir,
            title,
            inject_identity: req.inject_identity,
        }),
    ))
}

/// GET /api/v1/sessions/draft/{id} — read immutable metadata for a prepared link.
pub async fn prepared_get(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
) -> Result<Json<PreparedSessionResponse>> {
    if state.env_bridge.remote_draft_promoting(&draft_id).await {
        return Err(Error::SessionAlreadyRunning);
    }
    if let Some(spec) = state.env_bridge.remote_draft(&draft_id).await {
        return Ok(Json(PreparedSessionResponse {
            draft_id,
            environment: spec.environment,
            agent: spec.agent,
            workdir: spec.workdir,
            title: spec.title,
            inject_identity: spec.inject_identity,
        }));
    }
    // Draft handles are owned by the envagent that created them. A local
    // A stale remote draft must never be accepted as a second execution path.
    Err(Error::SessionNotFound)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSessionRequest {
    /// Complete replacement flag set. Absent leaves flags untouched; an empty map
    /// clears them.
    pub flags: Option<std::collections::HashMap<String, String>>,
    /// Display title. Absent leaves it untouched.
    pub title: Option<String>,
}

/// PATCH /api/v1/sessions/{id}
///
/// # Why flags are validated here rather than at turn time
///
/// `POST .../turns` filters the stored set against the agent's declared schema and
/// silently drops what it does not recognize. That is right there: a descriptor can
/// change under a session created before it, and the turn should still run. It is wrong
/// for a user action — a model chosen in the composer that the agent never declared
/// would be accepted, stored, dropped at turn time, and the turn would run on the
/// default with nothing anywhere saying why.
///
/// # Identity is not patchable, and three of the four fields that used to say so are
/// gone
///
/// `environment`, `agent`, and `nativeId` *are* the session's identity, so changing one
/// does not modify a session — it names a different one. `injectIdentity` is refused
/// for a different reason: a transcript records what produced it, so rewriting the flag
/// would rewrite what the stored history means. `origin`, `identityMode`, and
/// `lifecycleMode` no longer exist.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSessionRequest>,
) -> Result<Json<SessionResponse>> {
    let id = decode_id(&id)?;
    let _session = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    // Flags are session metadata. Persisting them does not execute an adapter
    // and remains available while an envagent is temporarily offline; the next
    // turn carries the stored set through the bridge.
    if let Some(flags) = &req.flags {
        let environments = state
            .env_bridge
            .list_environments()
            .await
            .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
        let info = environments
            .into_iter()
            .find(|environment| environment.id == id.environment)
            .and_then(|environment| {
                environment
                    .agents
                    .into_iter()
                    .find(|agent| agent.name == id.agent.as_str())
            })
            .and_then(|agent| agent.info);
        let Some(info) = info else {
            return Err(Error::InvalidRequest(format!(
                "Agent '{}' is unavailable on this environment.",
                id.agent
            )));
        };
        for (key, value) in flags {
            let Some(spec) = info.flags.get(key) else {
                return Err(Error::InvalidRequest(format!(
                    "Agent '{}' does not declare a '{}' flag.",
                    id.agent, key
                )));
            };
            if !spec.choices.is_empty() && !spec.choices.contains(value) {
                return Err(Error::InvalidRequest(format!(
                    "'{}' is not a valid {} for '{}'.",
                    value, key, id.agent
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

/// Resolve an environment id to a connected envagent, or refuse.
///
/// # Why this is a `503` and not a `400`
///
/// A well-formed environment id naming a machine no envagent is connected for is not a
/// malformed request — it may be exactly right, and the fix is to bring the machine
/// online (`docs/api/sessions.md`: "bring the machine online" is one of the six distinct
/// adopt fixes). Answering `400` would send a reader to their own request.
///
/// The check is against the bridge lease for every environment, including the
/// machine hosting the server. A local adapter fallback would violate the single
/// execution path and make a disconnected envagent appear healthy.
async fn require_environment(state: &AppState, requested: &str) -> Result<EnvironmentId> {
    let environment = EnvironmentId::parse(requested.trim())
        .map_err(|error| Error::InvalidRequest(error.to_string()))?;

    if !state.env_bridge.environment_connected(&environment).await {
        return Err(Error::EnvironmentUnreachable(environment.to_string()));
    }
    Ok(environment)
}

/// One native session an environment holds, as a picker renders it.
///
/// Flatter than [`sylph_protocol::NativeSummary`] by two additions the server is the
/// only holder of: whether Sylph already has this session, and whether its recorded
/// directory is one this environment permits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSessionRow {
    pub agent: String,
    pub native_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_native_id: Option<String>,
    /// Which relationship [`Self::parent_native_id`] is, when the vendor's key says.
    ///
    /// Sent because the pointer alone cannot be rendered: "forked from" and "delegated
    /// by" are the two relationships a listing must not show under one label
    /// (ADR-0001 § 2), and a picker holding only the id would have to pick one. Absent
    /// where the vendor named a parent without saying how, which is also the case in
    /// which no ancestry edge is journaled on adopt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_relation: Option<sylph_protocol::SessionEdgeRelation>,
    pub fidelity: sylph_protocol::Fidelity,
    /// The encoded triple this row *would* adopt as, always present.
    ///
    /// Sent even when adoption is refused, because it is what a client navigates to for
    /// an already-adopted row — showing the session rather than offering a duplicate.
    pub id: String,
    /// Why this row cannot be adopted, absent when it can.
    ///
    /// **A row is shown rather than hidden, and this is what makes that defensible.** A
    /// picker that silently omitted already-adopted sessions would leave a user hunting
    /// for a session that is plainly on the machine, and one that offered them would
    /// hand out a `409` nobody can act on. `docs/api/sessions.md`
    /// § `GET /api/v1/sessions/enumerate`: "A session already adopted is marked as such,
    /// so a picker can show it rather than offering a duplicate that would `409`."
    ///
    /// Typed rather than prose so a client branches on it — an `alreadyAdopted` row
    /// navigates, an `outsideRoots` row explains a configuration fix, and a `noWorkdir`
    /// row cannot be made adoptable at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<Blocked>,
}

/// Why a native session is not offerable.
///
/// Each variant is one of the adopt errors, decided *before* the request rather than
/// after — same classification, so a picker cannot disagree with the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Blocked {
    /// Sylph already has a row for this triple. Adopting again is `409`.
    AlreadyAdopted,
    /// The store recorded no directory. Adopting is `422` and no configuration fixes it.
    NoWorkdir,
    /// The recorded directory is outside this environment's roots. Adopting is `403`,
    /// and widening `paths.browse_roots` is the fix.
    OutsideRoots,
}

/// Whether a recorded workdir is one this environment permits, and if not, why.
///
/// Uses the same [`crate::paths::contain_in_any`] primitive `adopt` and both file APIs
/// use, shared rather than reimplemented — a containment rule written twice is one that
/// will diverge.
///
/// **Only the classification is taken; the resolved path is deliberately discarded.** The
/// path this listing *shows* is the vendor's own recorded spelling, because that is what a
/// user recognizes, and `adopt` re-derives its own canonical path from its own check
/// rather than trusting one this function produced. A containment check paired with a path
/// it did not produce is not a bound (`AGENTS.md`), so the two must not share one — this
/// answers "would adopt refuse this", and adopt answers "where does it run".
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumerateQuery {
    environment: String,
    agent: Option<String>,
    workdir: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumerateResponse {
    sessions: Vec<NativeSessionRow>,
    /// Pass as `cursor` for the next page. Absent when this page is the last.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// How many rows one page holds by default, and at most.
///
/// A picker renders a filterable list, and a real store holds sessions in the thousands —
/// so a whole-store page would be a megabyte of JSON to render a list whose first screen is
/// twenty rows.
const ENUMERATE_DEFAULT_LIMIT: usize = 50;
const ENUMERATE_MAX_LIMIT: usize = 200;

/// GET /api/v1/sessions/enumerate
///
/// What native sessions an environment holds, without adopting any of them
/// (`docs/api/sessions.md` § `GET /api/v1/sessions/enumerate`). This is what makes adopt
/// offerable in a UI rather than requiring a user to know a vendor id.
///
/// # Paging is server-side, because `StoreFilter` has no cursor
///
/// The spec names a `cursor` parameter and the adapter contract offers `workdir`,
/// `since_ms`, and `limit` — no cursor and no offset. `since_ms` cannot serve as one: it
/// selects activity *at or after* a mark while a listing runs newest-first, so paging
/// with it walks the wrong way.
///
/// So the cursor is an opaque offset into this listing, and the cost is stated rather
/// than hidden: each page re-enumerates from the top. That is affordable **because of what
/// `enumerate` is required to be** — one short bounded read per file rather than a parse,
/// which is orders of magnitude cheaper than reading each session's metadata
/// (`sylph_protocol::Adapter::enumerate`). Paging against the cheap operation is what that
/// requirement buys.
///
/// It is opaque by contract — a base32-ish decimal string a client echoes back, never
/// parses — so it can become a real per-adapter cursor without a client change.
///
/// # Ordering across agents
///
/// Each adapter answers newest-first for its own store, and a listing that named no
/// agent has to merge them: rows are re-sorted on the best timestamp each carries. A
/// session with **no** timestamp sorts last rather than first, because an absent stamp is
/// a store that has not settled, not a session from 1970 — and `Option`'s natural
/// ordering would put every one of them at the top of the first page.
pub async fn enumerate(
    State(state): State<AppState>,
    Query(query): Query<EnumerateQuery>,
) -> Result<Json<EnumerateResponse>> {
    let environment = require_environment(&state, &query.environment).await?;
    let limit = query
        .limit
        .unwrap_or(ENUMERATE_DEFAULT_LIMIT)
        .clamp(1, ENUMERATE_MAX_LIMIT);
    let offset: usize = match non_blank(query.cursor) {
        Some(cursor) => cursor.parse().map_err(|_| {
            Error::InvalidRequest("That cursor did not come from this endpoint.".to_string())
        })?,
        None => 0,
    };
    let workdir = non_blank(query.workdir).map(|path| crate::api::workspaces::normalize(&path));

    // Inventory is reported by the connected envagent. The server chooses no
    // adapter and never reads a native store itself.
    let inventory = state
        .env_bridge
        .list_environments()
        .await
        .map_err(|error| {
            Error::Internal(format!("could not read environment inventory: {error:#}"))
        })?
        .into_iter()
        .find(|snapshot| snapshot.id == environment)
        .ok_or_else(|| Error::EnvironmentUnreachable(environment.to_string()))?;
    let agents: Vec<String> = match non_blank(query.agent).as_deref() {
        Some(agent) => {
            if !inventory.agents.iter().any(|entry| entry.name == agent) {
                return Err(Error::InvalidRequest(format!(
                    "Unknown agent '{agent}' on this environment."
                )));
            }
            if !inventory
                .agents
                .iter()
                .any(|entry| entry.name == agent && entry.available)
            {
                return Err(Error::InvalidRequest(format!(
                    "Agent '{agent}' is not available on this environment."
                )));
            }
            vec![agent.to_string()]
        }
        None => inventory
            .agents
            .iter()
            .filter(|entry| entry.available)
            .map(|entry| entry.name.clone())
            .collect(),
    };

    let mut rows = Vec::new();
    for agent in agents {
        let request_id = sylph_protocol::CommandRequestId::parse(format!(
            "enumerate-{}-{}",
            agent,
            uuid::Uuid::new_v4().simple()
        ))
        .map_err(|error| Error::InvalidRequest(error.to_string()))?;
        let command = sylph_protocol::Command::new(
            request_id,
            sylph_protocol::IdempotencyPolicy::Replayable,
            sylph_protocol::EnvironmentCommand::Enumerate {
                agent: agent.clone(),
                workdir: workdir.clone(),
            },
        );
        let outcome = state
            .env_bridge
            .dispatch_and_wait(&environment, command, std::time::Duration::from_secs(30))
            .await
            .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
        let value = match outcome {
            sylph_protocol::CommandOutcome::Accepted {
                result: Some(value),
            } => value,
            sylph_protocol::CommandOutcome::Accepted { result: None } => serde_json::json!([]),
            sylph_protocol::CommandOutcome::Rejected { code, message } => {
                return Err(Error::LinkFailed(format!("{code}: {message}")))
            }
            sylph_protocol::CommandOutcome::Unknown { message } => {
                return Err(Error::LinkFailed(format!(
                    "remote enumeration outcome is unknown: {}",
                    message.unwrap_or_default()
                )))
            }
        };
        let summaries: Vec<sylph_protocol::NativeSummary> =
            serde_json::from_value(value).map_err(|error| {
                Error::Internal(format!("remote enumeration was malformed: {error}"))
            })?;
        let agent_name =
            AgentName::parse(agent.clone()).map_err(|error| Error::Internal(error.to_string()))?;
        let adopted = state.store.adopted_native_ids(&environment, &agent).await?;
        for summary in summaries {
            let id = SessionId::new(
                environment.clone(),
                agent_name.clone(),
                summary.native_id.clone(),
            );
            let blocked = if adopted.contains(summary.native_id.as_str()) {
                Some(Blocked::AlreadyAdopted)
            } else if summary.workdir.is_none() {
                Some(Blocked::NoWorkdir)
            } else {
                None
            };
            rows.push(NativeSessionRow {
                agent: agent.clone(),
                native_id: summary.native_id.to_string(),
                workdir: summary.workdir,
                title: summary.title,
                started_at_ms: summary.started_at_ms,
                ended_at_ms: summary.ended_at_ms,
                event_count: summary.event_count,
                parent_native_id: summary.parent_native_id.map(|id| id.to_string()),
                parent_relation: summary.parent_relation,
                fidelity: summary.fidelity,
                id: id.encode(),
                blocked,
            });
        }
    }
    rows.sort_by_key(|row| {
        std::cmp::Reverse(row.ended_at_ms.or(row.started_at_ms).unwrap_or(i64::MIN))
    });
    let total = rows.len();
    let page: Vec<_> = rows.into_iter().skip(offset).take(limit).collect();
    let next = offset + page.len();
    Ok(Json(EnumerateResponse {
        sessions: page,
        next_cursor: (next < total).then(|| next.to_string()),
    }))
}

/// POST /api/v1/sessions/adopt
///
/// Bring in a session an agent already has, from that agent's own native store.
///
/// # What is derived from the store rather than from the request
///
/// The **workdir**, the **fidelity**, and the fallback **title**. A client supplies the
/// triple, `watch`, and two settable preferences, and nothing else it sends decides
/// where an agent may run.
///
/// # The recorded workdir is bounded exactly like a created one
///
/// Not belt-and-braces. A machine's store holds every session any agent ever ran there,
/// so an unbounded adopt is a **general way to start an agent anywhere**: find a session
/// that ran in the target directory, adopt it, send a turn. That the path came from the
/// store rather than from the request changes who chose it, not what happens next — so
/// this goes through the envagent's own containment check, which canonicalizes on the
/// machine that owns the path. The server records the path returned by that command and
/// never checks a remote path against its own filesystem (`docs/architecture.md` § Filesystem exposure).
///
/// # `fidelity` is what the vendor's store holds, never `stream`
///
/// Read per session from the adapter's own summary rather than per agent, because a
/// store can hold both: a session truncated mid-write, or one whose early turns predate a
/// vendor's flush change, yields less than the agent's headline fidelity promises
/// (`sylph_protocol::NativeSummary::fidelity`). No care recovers a timing that was never
/// written.
///
/// # What the title fallback can and cannot reach, measured
///
/// The spec says the title "falls back to the opening user turn" through
/// [`crate::db::labels`]. That rule is implemented here and it **cannot see the opening
/// ask for two of the three agents**, which is a disagreement between the spec and the
/// parsers rather than a gap in this handler:
///
/// | Agent | Vendor title | The user's own text |
/// | --- | --- | --- |
/// | `claude` | ~93% of sessions | dropped by its parser |
/// | `codex` | never | dropped by its parser |
/// | `pi` | never | arrives as a `status` event |
///
/// Both parsers drop it citing the same reason — the vocabulary has no `user` kind and
/// "the server owns a turn's `request_text`" — but `SessionMessage` has three variants and
/// none of them carries user text, so there is nothing for the server to own it *from*.
/// The consequence is larger than a title: an adopted `claude` or `codex` transcript holds
/// the agent's half of the conversation and not the user's, and every turn's
/// `request_text` is empty. `crate::db::ingest` records the same finding from the
/// ingestion side.
///
/// So this reaches, in order: the client's override, the vendor's own recorded title, then
/// the opening phase-less `status` event through [`crate::db::labels::summarize_line`] —
/// which applies the envelope rule, because an agent's injected context arrives as leading
/// pseudo user turns and taking one verbatim labels most sessions with boilerplate that is
/// byte-identical across every session of that agent. The rule stays **narrow**: an earlier
/// version also treated a leading `You are ` as boilerplate and discarded 44 genuine
/// labels, because subagent briefs legitimately open that way.
///
/// # Ingestion, and why `watch` does not block the response
///
/// `watch: false` is drained **before** the `201`, so a client that reads the transcript
/// the moment it gets a response finds it there. `watch: true` follows the store as it
/// grows and has no end to wait for; the envagent owns that long-lived source and emits
/// its events over the bridge.
pub async fn adopt(
    State(state): State<AppState>,
    Json(req): Json<AdoptSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>)> {
    let environment = require_environment(&state, &req.environment).await?;
    let agent = req.agent.trim().to_string();

    let requested_agent = AgentName::parse(agent.clone())
        .map_err(|error| Error::InvalidRequest(error.to_string()))?;
    let requested_native = NativeId::new(req.native_id.trim());
    let requested_session_id = SessionId::new(
        environment.clone(),
        requested_agent.clone(),
        requested_native.clone(),
    );
    let _session_command_guard = state
        .env_bridge
        .session_command_lock(&requested_session_id)
        .await
        .lock_owned()
        .await;
    if req.native_id.trim().is_empty() {
        return Err(Error::InvalidRequest(
            "Adopting a session needs the agent's own session id.".into(),
        ));
    }
    let remote_session_id = requested_session_id.clone();
    if state.store.get_session(&remote_session_id).await?.is_some() {
        return Err(Error::SessionAlreadyPresent);
    }
    let command_payload = serde_json::json!({
        "environment": environment,
        "agent": agent,
        "nativeId": req.native_id.trim(),
        "watch": req.watch,
    });
    let request_id = sylph_protocol::CommandRequestId::parse(format!(
        "adopt-{}",
        blake3::hash(command_payload.to_string().as_bytes()).to_hex()
    ))
    .map_err(|error| Error::InvalidRequest(error.to_string()))?;
    let native_id_for_command = remote_session_id.native_id.clone();
    let command_kind = if req.watch {
        sylph_protocol::EnvironmentCommand::Watch {
            session_id: SessionId::new(
                environment.clone(),
                AgentName::parse(agent.clone())
                    .map_err(|error| Error::InvalidRequest(error.to_string()))?,
                native_id_for_command.clone(),
            ),
            agent: agent.clone(),
            native_id: native_id_for_command.clone(),
            turn_id: None,
        }
    } else {
        sylph_protocol::EnvironmentCommand::Adopt {
            session_id: SessionId::new(
                environment.clone(),
                AgentName::parse(agent.clone())
                    .map_err(|error| Error::InvalidRequest(error.to_string()))?,
                native_id_for_command.clone(),
            ),
            agent: agent.clone(),
            native_id: native_id_for_command.clone(),
            turn_id: None,
        }
    };
    let command = sylph_protocol::Command::new(
        request_id,
        sylph_protocol::IdempotencyPolicy::AtMostOnce,
        command_kind,
    );
    let outcome = state
        .env_bridge
        .dispatch_and_wait(&environment, command, std::time::Duration::from_secs(30))
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    let result = match outcome {
        sylph_protocol::CommandOutcome::Accepted {
            result: Some(value),
        } => value,
        sylph_protocol::CommandOutcome::Accepted { result: None } => {
            return Err(Error::LinkFailed("remote adopt returned no session".into()))
        }
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            return Err(match code.as_str() {
                "NATIVE_SESSION_NOT_FOUND" | "ADOPT_FAILED" => {
                    Error::NativeSessionNotFound(native_id_for_command.to_string())
                }
                "AGENT_UNKNOWN" => Error::InvalidRequest(format!("Unknown agent: {agent}")),
                _ => Error::InvalidRequest(format!("{code}: {message}")),
            })
        }
        sylph_protocol::CommandOutcome::Unknown { message } => {
            return Err(Error::LinkFailed(format!(
                "remote adopt outcome is unknown: {}",
                message.unwrap_or_default()
            )))
        }
    };
    let session_id: SessionId = serde_json::from_value(
        result
            .get("sessionId")
            .cloned()
            .ok_or_else(|| Error::LinkFailed("remote adopt returned no session id".into()))?,
    )
    .map_err(|error| Error::LinkFailed(format!("remote adopt returned invalid id: {error}")))?;
    if session_id != requested_session_id {
        return Err(Error::LinkFailed(
            "remote adopt returned a different session identity".into(),
        ));
    }
    // Native history may not record a working directory. Preserve that fact as
    // NULL; guessing a path would claim where future agent code may run.
    let workdir = result
        .get("workdir")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let now = now_ms();
    let record = Session {
        id: session_id.clone(),
        workdir,
        title: result
            .get("title")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        inject_identity: req.inject_identity,
        fidelity: result
            .get("fidelity")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or(sylph_protocol::Fidelity::Messages),
        flags_json: "{}".into(),
        status: "idle".into(),
        // Lineage is a typed edge, written after the row exists (ADR-0001 § 2).
        created_at_ms: result
            .get("startedAtMs")
            .and_then(|v| v.as_i64())
            .unwrap_or(now),
        updated_at_ms: result
            .get("endedAtMs")
            .and_then(|v| v.as_i64())
            .unwrap_or(now),
    };
    let stored = match state.store.create_session(&record).await {
        Ok(stored) => stored,
        Err(error) => {
            let _ = state.env_bridge.close_remote_session(&session_id).await;
            return Err(error);
        }
    };
    record_adopted_lineage(
        &state,
        &session_id,
        result
            .get("parentNativeId")
            .and_then(|value| value.as_str())
            .map(NativeId::new)
            .as_ref(),
        result
            .get("parentRelation")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    )
    .await;
    if let Err(error) = state.env_bridge.activate_remote(&session_id).await {
        let _ = state.env_bridge.close_remote_session(&session_id).await;
        return Err(Error::LinkFailed(format!("{error:#}")));
    }
    Ok((
        StatusCode::CREATED,
        Json(SessionResponse { session: stored }),
    ))
}

/// What one re-read pass added, as the endpoint reports it.
///
/// Four counts rather than a boolean, because the interesting states are not "worked" and
/// "failed". A pass that examined 412 events and added none is the ordinary answer to
/// "anything new?", and it must be distinguishable from a pass that could not read the
/// store at all — which is an error with its own status, never a quiet zero
/// (`docs/api/sessions.md` § `POST /api/v1/sessions/{id}/reread`).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RereadReport {
    /// Events the adapter emitted, before suppression.
    ///
    /// **`ingested + already_held` may be less than this, by design.** A turn's opening
    /// bracket survives in neither storage tier, so a pass that finds nothing new after
    /// one declines to write it and cannot claim it was already held either — it is
    /// examined and nothing else (`crate::db::IngestReport::already_held`).
    pub examined: usize,
    /// Events new to the journal, now added.
    pub ingested: usize,
    /// Events a previous pass had already journaled.
    pub already_held: usize,
    /// Items the adapter could not read. The pass continues past each.
    pub read_errors: usize,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RereadResponse {
    pub reread: RereadReport,
}

/// POST /api/v1/sessions/{id}/reread
///
/// Re-parse the vendor's native store for a session Sylph already holds, ingesting
/// whatever the journal lacks. One pass, on demand.
///
/// # This is not adopt, and not watch
///
/// `adopt` refuses a session that already has a row — `SESSION_ALREADY_PRESENT` is exactly
/// this case — and `watch` installs a follower that lives past the response. A re-read
/// targets a row that exists, runs once, and leaves no follower behind.
///
/// # `fidelity` is not touched
///
/// A record's resolution belongs to the source that wrote it, so re-reading the same store
/// cannot raise or lower it. Nothing here writes the column, and that is the assertion:
/// re-deriving it from a second pass would let a partial read downgrade a session, which a
/// renderer reads as permission to stop drawing a waterfall it had been drawing.
pub async fn reread(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RereadResponse>> {
    let session_id = decode_id(&id)?;
    // The row must exist: this operation extends a transcript rather than creating one.
    // `404` distinguishes "you named nothing" from the *other* `404` below, which is the
    // vendor having dropped what Sylph still holds.
    state
        .store
        .get_session(&session_id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    // One re-read at a time per session. Two concurrent passes would each read the same
    // suppression snapshot, so both would find the same tail new and both would write it —
    // producing the duplicate the snapshot exists to prevent, and only under a double
    // click.
    let _guard = state
        .env_bridge
        .session_command_lock(&session_id)
        .await
        .lock_owned()
        .await;

    let agent = session_id.agent.to_string();

    // Unconditionally through the bridge. Milestone 1 deleted the in-process
    // adapter seam, so there is no privileged machine here -- not even the one
    // the server runs on (`docs/environments.md` § Self-installed). The local
    // branch this replaced could not compile against that layout.
    let outcome = state
        .env_bridge
        .remote_session_command(
            &session_id,
            sylph_protocol::EnvironmentCommand::Reread {
                session_id: session_id.clone(),
                agent: agent.clone(),
                native_id: session_id.native_id.clone(),
            },
            // Never `Replayable`. A re-read exists to observe a store that has changed
            // since it was last asked, so a replayed answer is a stale one presented as
            // fresh.
            sylph_protocol::IdempotencyPolicy::AtMostOnce,
        )
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    let value = match outcome {
        sylph_protocol::CommandOutcome::Accepted { result } => result.unwrap_or_default(),
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            // The envagent could not find the session in the vendor's store. That is
            // the store having been pruned under a row Sylph still holds, which has its
            // own code because it means Sylph's copy is now the only one.
            if message.contains("was not found") {
                return Err(Error::NativeSessionNotFound(
                    session_id.native_id.as_str().to_string(),
                ));
            }
            return Err(Error::Internal(format!("{code}: {message}")));
        }
        sylph_protocol::CommandOutcome::Unknown { message } => {
            return Err(Error::LinkFailed(format!(
                "remote re-read outcome is unknown: {}",
                message.unwrap_or_default()
            )))
        }
    };
    let pass: sylph_protocol::RereadPass = serde_json::from_value(value)
        .map_err(|error| Error::Internal(format!("remote re-read was malformed: {error}")))?;
    Ok(Json(ingest_reread_pass(&state, &session_id, pass).await?))
}

/// Ingest a pass an envagent parsed.
///
/// The whole of [`reread`]'s ingest, since every environment reaches its adapters through
/// an envagent. The envagent answers with the whole pass because suppression is the
/// server's — only the server knows what its journal holds — so the messages are
/// re-assembled into a stream and put through one ingest path. A second path for the same
/// operation would be a second place for the container, sequence, and dedup rules to
/// diverge.
async fn ingest_reread_pass(
    state: &AppState,
    session_id: &sylph_protocol::SessionId,
    pass: sylph_protocol::RereadPass,
) -> Result<RereadResponse> {
    let read_errors = pass.read_errors;
    // Records first, then events, in the order the adapter emitted them: a native record is
    // the source of the events projected from it, and normalized events are never kept
    // without it (`docs/storage.md` § Native records are kept whole).
    let messages: Vec<_> = pass
        .records
        .into_iter()
        .map(sylph_protocol::SessionMessage::NativeRecord)
        .chain(
            pass.events
                .into_iter()
                .map(sylph_protocol::SessionMessage::Event),
        )
        .map(Ok)
        .collect();
    let stream: sylph_protocol::EventStream = Box::pin(futures::stream::iter(messages));
    let report = state
        .ingest
        .reread(session_id, stream)
        .await
        .map_err(|error| {
            Error::Internal(format!(
                "Could not retain that native session losslessly: {error:#}"
            ))
        })?;
    Ok(RereadResponse {
        reread: RereadReport {
            // The pass's own count, not `events + already_held`. A turn opener the pass
            // declined to write is in neither of those, so the sum would under-report the
            // store by however many brackets bracketed nothing
            // (`crate::db::IngestReport::examined`).
            examined: report.examined,
            ingested: report.events,
            already_held: report.already_held,
            // The envagent's own read errors, not this ingest's: the parse happened there,
            // so that is where an unreadable record was seen.
            read_errors: read_errors + report.read_errors,
        },
    })
}
