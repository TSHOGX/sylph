//! Native session metadata and environment-owned session lifecycle.
use crate::{
    db::SessionFilter,
    env::bridge::RemoteCommandError,
    error::{Error, Result},
    middleware::auth::SubjectId,
    models::{now_ms, AdoptSessionRequest, CreateSessionRequest, Session},
    AppState,
};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sylph_protocol::trace::{AgentRef, IngestionScope};
use sylph_protocol::{
    AgentName, CaptureAuthorization, EnvironmentCommand, EnvironmentId, IdempotencyPolicy,
    NativeId, SessionId,
};

pub(crate) async fn authorize(
    state: &AppState,
    subject: &str,
    id: &SessionId,
    capability: &str,
) -> Result<()> {
    if !state
        .store
        .has_capability(subject, id.environment.as_str(), capability)
        .await?
    {
        return Err(Error::Forbidden(format!(
            "{capability} capability required"
        )));
    }
    Ok(())
}

pub async fn adopt(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Json(request): Json<AdoptSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>)> {
    let environment = require_environment(&state, &request.environment).await?;
    let agent =
        AgentName::parse(&request.agent).map_err(|e| Error::InvalidRequest(e.to_string()))?;
    if request.native_id.trim().is_empty() {
        return Err(Error::InvalidRequest(
            "native identity must not be empty".into(),
        ));
    }
    let id = SessionId::new(environment, agent, NativeId::new(&request.native_id));
    authorize(&state, &subject.0, &id, "observe").await?;
    if state.store.get_session(&id).await?.is_some() {
        return Err(Error::SessionAlreadyPresent);
    }
    let scope = IngestionScope::Agent(AgentRef::Session(id.clone()));
    let epoch = match state
        .trace
        .catalogue()
        .evidence()
        .authorize_scope(&scope, &[AgentRef::Session(id.clone())])
        .await
    {
        Ok(epoch) => epoch,
        Err(crate::trace::evidence::EvidenceError::Suppressed) => state
            .trace
            .catalogue()
            .evidence()
            .readopt_scope(&scope, &[AgentRef::Session(id.clone())])
            .await
            .map_err(|e| Error::Internal(e.to_string()))?,
        Err(error) => return Err(Error::Internal(error.to_string())),
    };
    let capture_authorization = CaptureAuthorization { scope, epoch };
    let command = if request.watch {
        EnvironmentCommand::Watch {
            capture_authorization,
            session_id: id.clone(),
            agent: request.agent.clone(),
            native_id: id.native_id.clone(),
            turn_id: None,
        }
    } else {
        EnvironmentCommand::Adopt {
            capture_authorization,
            session_id: id.clone(),
            agent: request.agent.clone(),
            native_id: id.native_id.clone(),
            turn_id: None,
        }
    };
    let outcome = state
        .env_bridge
        .remote_session_command(&id, command, IdempotencyPolicy::AtMostOnce)
        .await
        .map_err(|e| Error::LinkFailed(e.to_string()))?;
    let value = crate::env::EnvBridge::remote_outcome_value(outcome)
        .await
        .map_err(|e| Error::LinkFailed(e.to_string()))?;
    let workdir = value
        .get("workdir")
        .and_then(|v| v.as_str())
        .ok_or(Error::NativeSessionNotResumable)?
        .to_owned();
    let title = request.title.or_else(|| {
        value
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    });
    let fidelity = value
        .get("fidelity")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| Error::Internal(e.to_string()))?
        .unwrap_or(sylph_protocol::Fidelity::Messages);
    let session = Session {
        id,
        workdir: Some(workdir),
        title,
        inject_identity: request.inject_identity,
        fidelity,
        flags_json: "{}".into(),
        status: "idle".into(),
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
    };
    let session = state.store.create_session(&session).await?;
    state.trace.notify_admission();
    Ok((StatusCode::CREATED, Json(SessionResponse { session })))
}

pub async fn reread(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let id = decode_id(&id)?;
    authorize(&state, &subject.0, &id, "observe").await?;
    state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    let scope = IngestionScope::Agent(AgentRef::Session(id.clone()));
    let epoch = state
        .trace
        .catalogue()
        .evidence()
        .authorize_scope(&scope, &[AgentRef::Session(id.clone())])
        .await
        .map_err(|e| Error::Internal(e.to_string()))?;
    let command = EnvironmentCommand::Reread {
        capture_authorization: CaptureAuthorization { scope, epoch },
        session_id: id.clone(),
        agent: id.agent.to_string(),
        native_id: id.native_id.clone(),
    };
    let outcome = state
        .env_bridge
        .remote_session_command(&id, command, IdempotencyPolicy::AtMostOnce)
        .await
        .map_err(|e| Error::LinkFailed(e.to_string()))?;
    let result = crate::env::EnvBridge::remote_outcome_value(outcome)
        .await
        .map_err(|e| Error::LinkFailed(e.to_string()))?;
    state.trace.notify_admission();
    Ok(Json(result))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let id = decode_id(&id)?;
    authorize(&state, &subject.0, &id, "control").await?;
    let lock = state.env_bridge.session_command_lock(&id).await;
    let _guard = lock.lock().await;
    state
        .trace
        .catalogue()
        .evidence()
        .delete_scope(&IngestionScope::Agent(AgentRef::Session(id.clone())))
        .await
        .map_err(|e| Error::Internal(e.to_string()))?;
    state.trace.notify_access_changed();
    state
        .exporter
        .run_once()
        .await
        .map_err(|e| Error::Internal(e.to_string()))?;
    state
        .store
        .erase_agent_input(&AgentRef::Session(id.clone()))
        .await
        .map_err(|e| Error::Internal(e.to_string()))?;
    state.store.suppress_session(&id).await?;
    let _ = state.env_bridge.close_remote_session(&id).await;
    state.trace.notify_admission();
    Ok(StatusCode::NO_CONTENT)
}
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
        capability_subject: Some(subject.0.clone()),
        capability: Some("observe".to_string()),
    };
    let sessions = state.store.list_sessions(&filter).await?;
    Ok(Json(SessionListResponse { sessions }))
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub session: Session,
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

pub async fn create(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<PreparedSessionResponse>)> {
    let environment = require_environment(&state, &req.environment).await?;
    if !state
        .store
        .has_capability(&subject.0, environment.as_str(), "control")
        .await?
    {
        return Err(Error::Forbidden("control capability required".into()));
    }
    // All new sessions use the same prepared lifecycle. There is no eager-create
    // compatibility path: a session becomes durable only after its first real turn.
    create_prepared(State(state), subject, Json(req)).await
}

/// Start an agent without durable history for POST /api/v1/sessions.
///
/// This endpoint is the draft half of the two-phase lifecycle.  It deliberately
/// returns only an opaque handle; no `sessions` row is written until the first turn
/// promotes the prepared link.
async fn create_prepared(
    State(state): State<AppState>,
    subject: SubjectId,
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
            subject.0.clone(),
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
    Extension(subject): Extension<SubjectId>,
    Path(draft_id): Path<String>,
) -> Result<Json<PreparedSessionResponse>> {
    if state.env_bridge.remote_draft_promoting(&draft_id).await {
        return Err(Error::SessionAlreadyRunning);
    }
    let spec = state
        .env_bridge
        .authorize_remote_draft(&draft_id, &subject.0, "observe")
        .await
        .map_err(|error| {
            let message = format!("{error:#}");
            if message.contains("not found") {
                Error::SessionNotFound
            } else {
                Error::Forbidden(message)
            }
        })?;
    Ok(Json(PreparedSessionResponse {
        draft_id,
        environment: spec.environment,
        agent: spec.agent,
        workdir: spec.workdir,
        title: spec.title,
        inject_identity: spec.inject_identity,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
    Extension(subject): Extension<SubjectId>,
    Query(query): Query<EnumerateQuery>,
) -> Result<Json<EnumerateResponse>> {
    let environment = require_environment(&state, &query.environment).await?;
    if !state
        .store
        .has_capability(&subject.0, environment.as_str(), "observe")
        .await?
    {
        return Err(Error::Forbidden("observe capability required".into()));
    }
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
