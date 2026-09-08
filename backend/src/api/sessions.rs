//! Sessions: listing, reading, starting, adopting, and patching.

use crate::models::now_ms;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sylph_protocol::{AgentName, EnvironmentId, NativeId, SessionId};

use crate::{
    db::store::SessionFilter,
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
    Path(id): Path<String>,
) -> Result<Json<SessionResponse>> {
    let id = decode_id(&id)?;
    let session = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
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

/// Resolve a client-named working directory, or refuse it.
///
/// # Why naming a workdir is bounded at all
///
/// A session's workdir is the `current_dir` of a spawned agent process, and a coding
/// agent's whole job is reading and writing the tree it is pointed at. So this string
/// decides where arbitrary code runs — existence is the one property that says nothing
/// about whether it *should*. Before this check, any client of this backend could start
/// an agent anywhere on the filesystem. `browse_roots` bounded what `/api/v1/fs/*` may
/// *enumerate* and said nothing about what a request may *name*, which meant the
/// narrower capability was bounded and the wider one was not.
///
/// # The rule
///
/// Canonicalize, then require containment in one of
/// [`Config::session_roots`](crate::config::Config::session_roots). Canonicalizing is
/// what defeats `..` and symlinks — including a link that lives on disk, which no
/// amount of inspecting the request string can see — and it is the same [`crate::paths`]
/// primitive the two file APIs use, deliberately shared rather than reimplemented.
///
/// The canonical path is what gets journalled, not the string as sent. Two spellings of
/// one directory (`/tmp/x` and `/private/tmp/x` on macOS) would otherwise be two
/// sidebar groups for one workspace.
///
/// # What this deliberately does *not* do
///
/// It is not applied to sessions already recorded. Nothing re-checks a journalled
/// workdir when a session is read, listed, or resumed: a rule that retroactively
/// orphaned real sessions — making them unreadable because configuration changed after
/// they were created — would be a worse bug than the hole it closes.
///
/// **With environments this check belongs in the envagent**, evaluated against that
/// machine's own configuration: only the machine holding a path can canonicalize it,
/// resolve `..`, or see whether a component is a symlink, so a server checking a remote
/// path would be matching a string against a filesystem it cannot see. It runs here
/// while `local` is the only environment.
fn contain_workdir(state: &AppState, requested: &str) -> Result<String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err(Error::InvalidRequest(
            "A session needs a working directory.".to_string(),
        ));
    }

    let roots = state.config.session_roots();
    if roots.is_empty() {
        // Every root is missing or unreadable. Refusing is the only honest answer — an
        // empty root set that fell through to "allow" would silently remove the bound
        // exactly when configuration is broken.
        return Err(Error::Forbidden(
            "No directory is configured for sessions to run in. Set `paths.browse_roots` \
             in config.toml."
                .to_string(),
        ));
    }

    let path = std::path::Path::new(requested);
    let target = crate::paths::contain_in_any(&roots, path, "the directories sessions may run in")
        .map_err(|error| match error {
            // `FileNotFound` is a 404, which is wrong for a request *body* field: the
            // session is not what is missing. Restated as the 400 it is, in words the
            // picker can show against the field that caused it.
            Error::FileNotFound => Error::InvalidRequest(format!(
                "That working directory does not exist: {requested}"
            )),
            other => other,
        })?;

    if !target.is_dir() {
        return Err(Error::InvalidRequest(format!(
            "A session's working directory must be a directory: {requested}"
        )));
    }

    Ok(target.display().to_string())
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
    // Paired environments are started through the real envagent bridge. The
    // in-process link remains only for the local development environment and
    // deterministic fixture tests.
    // Parsing alone is not enough: it would let a well-formed id for an unreachable
    // machine start a process locally and stamp the resulting session with a foreign
    // identity. Creation and adoption must share the same environment gate.
    let environment = require_environment(&state, &req.environment).await?;
    if environment.as_str() != state.environment.descriptor().id
        && state.env_bridge.environment_connected(&environment).await
    {
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
            .map_err(|error| Error::AgentStartFailed(anyhow::anyhow!("{error:#}")))?;
        return Ok((
            StatusCode::CREATED,
            Json(PreparedSessionResponse {
                draft_id,
                environment,
                agent: req.agent,
                workdir,
                title,
                inject_identity: req.inject_identity,
            }),
        ));
    }
    if !state
        .adapters
        .available()
        .iter()
        .any(|name| name == &req.agent)
    {
        return Err(Error::InvalidRequest(format!(
            "Unknown agent: {}",
            req.agent
        )));
    }
    let workdir = contain_workdir(&state, &req.workdir)?;
    let agent_name = req.agent.clone();
    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let inject_identity = req.inject_identity;
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
    let draft_id = state
        .links
        .prepare_handle(crate::links::OpenSpec {
            environment: environment.clone(),
            agent: agent_name.clone(),
            workdir: workdir.clone(),
            title: title.clone(),
            inject_identity,
            flags,
        })
        .await
        .map_err(Error::AgentStartFailed)?;
    Ok((
        StatusCode::CREATED,
        Json(PreparedSessionResponse {
            draft_id,
            environment,
            agent: agent_name,
            workdir,
            title,
            inject_identity,
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
    let spec = state
        .links
        .prepared_spec(&draft_id)
        .await
        .ok_or(Error::SessionNotFound)?;
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
    let session = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    if let Some(flags) = &req.flags {
        // A session whose agent this build cannot run has no schema to validate against,
        // so its flags cannot be changed. Failing is better than storing an unvalidated
        // value that would be dropped at turn time with nothing saying why.
        if !state
            .adapters
            .available()
            .iter()
            .any(|name| name == session.id.agent.as_str())
        {
            return Err(Error::InvalidRequest(format!(
                "Agent '{}' is not available in this build, so its flags cannot be \
                 changed.",
                session.id.agent
            )));
        }

        // The schema comes from a **live link**, because that is the only thing that
        // holds it: an agent's flags are what its CLI reported at handshake, not
        // something a descriptor beside it declares. Establishing one here is the cost
        // of validating on write, and validating on write is the whole point — see this
        // function's doc comment for why dropping an undeclared flag at turn time is a
        // silent no-op the UI cannot explain.
        //
        // A link that will not come up is *not* a refusal: the session is still valid and
        // the flag may be perfectly good, so this falls through to storing it rather than
        // turning an agent that is temporarily unreachable into a rejected user action.
        if let Ok(link) = state.links.ensure(&id).await {
            let schema = &link.info().flags;
            for (key, value) in flags {
                let Some(spec) = schema.get(key) else {
                    return Err(Error::InvalidRequest(format!(
                        "Agent '{}' does not declare a '{key}' flag.",
                        session.id.agent
                    )));
                };
                // An empty `choices` list means the flag is free-form, so only enumerated
                // flags are checked against their enumeration.
                if !spec.choices.is_empty() && !spec.choices.contains(value) {
                    return Err(Error::InvalidRequest(format!(
                        "'{value}' is not a valid {key} for '{}'. Valid choices: {:?}",
                        session.id.agent, spec.choices
                    )));
                }
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

/// Resolve an environment id to the adapter set that answers for it, or refuse.
///
/// # Why this is a `503` and not a `400`
///
/// A well-formed environment id naming a machine no envagent is connected for is not a
/// malformed request — it may be exactly right, and the fix is to bring the machine
/// online (`docs/api/sessions.md`: "bring the machine online" is one of the six distinct
/// adopt fixes). Answering `400` would send a reader to their own request.
///
/// Only the local environment exists until pairing lands
/// (`docs/roadmap.md` § Milestone 1), so anything else is unreachable by
/// construction rather than by a connection check. That is the honest answer *now* and
/// stays the honest answer afterwards, when the check becomes "is that envagent's
/// connection live".
async fn require_environment(state: &AppState, requested: &str) -> Result<EnvironmentId> {
    let environment = EnvironmentId::parse(requested.trim())
        .map_err(|error| Error::InvalidRequest(error.to_string()))?;

    let local = state.environment.descriptor().id;
    if environment.as_str() != local && !state.env_bridge.environment_connected(&environment).await
    {
        return Err(Error::EnvironmentUnreachable(environment.to_string()));
    }
    Ok(environment)
}

/// Which agents to read a native store from, for a request that named one or none.
///
/// Naming an agent whose CLI is absent is a `400` rather than an empty listing: an
/// empty result is indistinguishable from "that agent has no sessions here", and the
/// two have different fixes. The message names what this machine can read, the same way
/// `create` does — one wording for one class of mistake.
///
/// **Read the [`crate::adapters::AdapterSource`], never the registry directly.** Both
/// answer the same question — availability is `PATH` alone — but the integration suite
/// injects its agents into the source, so a direct call reaches the compiled-in registry
/// and cannot see them. `create` and `adopt` must agree on this or one is bounded and the
/// other is not.
///
/// An agent with no readable native store says so by refusing `adopt` and enumerating
/// empty, which is a property of the adapter rather than of this lookup.
fn require_agents(
    adapters: &dyn crate::adapters::AdapterSource,
    requested: Option<&str>,
) -> Result<Vec<String>> {
    let installed = adapters.available();

    match requested {
        Some(agent) => {
            if installed.iter().any(|name| name == agent) {
                Ok(vec![agent.to_string()])
            } else if adapters.is_known(agent) {
                // Known but absent: an install, which names a fix. Kept distinct from an
                // unsupported agent for the reason `crate::adapters::is_known` exists.
                Err(Error::InvalidRequest(format!(
                    "'{agent}' is not installed on this environment, so it has no native \
                     store to read. Installed here: {installed:?}."
                )))
            } else {
                Err(Error::InvalidRequest(format!(
                    "Unknown agent: {agent}. This build supports: {:?}.",
                    adapters.known_agents()
                )))
            }
        }
        None => Ok(installed),
    }
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
fn classify_workdir(roots: &[std::path::PathBuf], recorded: &str) -> Option<Blocked> {
    match crate::paths::contain_in_any(
        roots,
        std::path::Path::new(recorded),
        "the directories sessions may run in",
    ) {
        Ok(_) => None,
        // The directory is gone. Not `outsideRoots`: widening configuration would not
        // bring it back, and adopt answers `400` rather than `403` for it.
        Err(Error::FileNotFound) => Some(Blocked::NoWorkdir),
        Err(_) => Some(Blocked::OutsideRoots),
    }
}

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
    if environment.as_str() != state.environment.descriptor().id {
        let agent = non_blank(query.agent.clone()).unwrap_or_else(|| "".into());
        if agent.is_empty() {
            return Err(Error::InvalidRequest(
                "Remote enumeration requires an agent filter.".into(),
            ));
        }
        let request_id = sylph_protocol::CommandRequestId::parse(format!(
            "enumerate-{}",
            uuid::Uuid::new_v4().simple()
        ))
        .map_err(|error| Error::InvalidRequest(error.to_string()))?;
        let command = sylph_protocol::Command::new(
            request_id,
            sylph_protocol::IdempotencyPolicy::Replayable,
            sylph_protocol::EnvironmentCommand::Enumerate {
                agent: agent.clone(),
                workdir: non_blank(query.workdir.clone()),
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
                return Err(Error::InvalidRequest(format!("{code}: {message}")))
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
        let offset = query
            .cursor
            .as_deref()
            .and_then(|cursor| cursor.parse::<usize>().ok())
            .unwrap_or(0);
        let limit = query
            .limit
            .unwrap_or(ENUMERATE_DEFAULT_LIMIT)
            .clamp(1, ENUMERATE_MAX_LIMIT);
        let total = summaries.len();
        let rows = summaries
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|summary| {
                let agent_name = AgentName::parse(agent.clone())
                    .map_err(|error| Error::Internal(error.to_string()))?;
                Ok(NativeSessionRow {
                    id: SessionId::new(environment.clone(), agent_name, summary.native_id.clone())
                        .encode(),
                    agent: agent.clone(),
                    native_id: summary.native_id.to_string(),
                    workdir: summary.workdir,
                    title: summary.title,
                    started_at_ms: summary.started_at_ms,
                    ended_at_ms: summary.ended_at_ms,
                    event_count: summary.event_count,
                    parent_native_id: summary.parent_native_id.map(|id| id.to_string()),
                    fidelity: summary.fidelity,
                    blocked: None,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = (offset + rows.len() < total).then(|| (offset + rows.len()).to_string());
        return Ok(Json(EnumerateResponse {
            sessions: rows,
            next_cursor,
        }));
    }
    let agents = require_agents(state.adapters.as_ref(), non_blank(query.agent).as_deref())?;

    let limit = query
        .limit
        .unwrap_or(ENUMERATE_DEFAULT_LIMIT)
        .clamp(1, ENUMERATE_MAX_LIMIT);
    let offset: usize = match non_blank(query.cursor) {
        Some(cursor) => cursor.parse().map_err(|_| {
            // A cursor a client did not get from this endpoint. Refused rather than
            // treated as zero, because silently restarting a paged scan is how a client
            // loops forever over page one.
            Error::InvalidRequest("That cursor did not come from this endpoint.".to_string())
        })?,
        None => 0,
    };

    // Normalized here so an exact match compares the same spelling the workspace
    // grouping and the session listing use. `StoreFilter::workdir` is an **exact match
    // after normalizing trailing slashes, never a substring** — the store this replaced
    // compiled it to `LIKE '%…%'`, so `/w/app` also matched `/w/app-old`.
    let workdir = non_blank(query.workdir).map(|path| crate::api::workspaces::normalize(&path));

    let filter = sylph_protocol::StoreFilter {
        workdir: workdir.clone(),
        since_ms: None,
        // Deliberately unbounded per adapter rather than `Some(limit)`. The page is cut
        // after merging, and a per-adapter bound would silently drop a newer `pi` session
        // because `claude` happened to fill the page — a listing that is wrong in a way
        // nothing on screen explains.
        limit: None,
    };

    let mut rows: Vec<NativeSessionRow> = Vec::new();
    let roots = state.config.session_roots();
    // Distinct workdir → its classification. Held across agents because a directory is
    // frequently worked in by more than one of them.
    let mut containment: std::collections::HashMap<String, Option<Blocked>> =
        std::collections::HashMap::new();

    for agent in &agents {
        let Some(adapter) = state.adapters.for_agent(agent) else {
            // Raced an uninstall between the availability check and here. Skipping is
            // right: the other agents' sessions are still readable.
            continue;
        };
        // A snapshot, for two reasons that are both load-bearing — see
        // `api::native_index`. Enumerating this machine costs ~5.6 s measured, so paging
        // would re-pay it per page; and an offset cursor into a *live* store silently
        // skips and duplicates rows as sessions gain activity and move to the front.
        let key = crate::api::native_index::NativeIndex::key(
            environment.as_str(),
            agent,
            workdir.as_deref(),
        );
        let summaries = match state.native_index.get(&key).await {
            Some(cached) => cached,
            None => match adapter.enumerate(&filter).await {
                Ok(summaries) => {
                    let summaries = std::sync::Arc::new(summaries);
                    state
                        .native_index
                        .put(key, std::sync::Arc::clone(&summaries))
                        .await;
                    summaries
                }
                Err(error) => {
                    // One unreadable store must not empty the listing. Reported at `warn`
                    // once per request rather than per session, which is the
                    // degraded-dependency shape `AGENTS.md` records at 2339 lines.
                    tracing::warn!("Could not enumerate {}'s native store: {:#}", agent, error);
                    continue;
                }
            },
        };

        // One query per agent rather than one per row. Measured, the per-row shape costs
        // seconds on a real store — the round-trip count, not the query — and it hands back
        // exactly the affordability that `Adapter::enumerate` being a cheap listing pass was
        // supposed to buy.
        let adopted = state.store.adopted_native_ids(&environment, agent).await?;

        for summary in summaries.iter() {
            let id = SessionId::new(
                environment.clone(),
                AgentName::parse(agent.clone())
                    .map_err(|error| Error::Internal(error.to_string()))?,
                summary.native_id.clone(),
            );

            let blocked = if adopted.contains(summary.native_id.as_str()) {
                Some(Blocked::AlreadyAdopted)
            } else {
                match &summary.workdir {
                    None => Some(Blocked::NoWorkdir),
                    // Memoized per distinct path, because a real store repeats them
                    // heavily — one project directory holds hundreds of sessions — and
                    // each miss is a `canonicalize` syscall per configured root.
                    Some(recorded) => *containment
                        .entry(recorded.clone())
                        .or_insert_with(|| classify_workdir(&roots, recorded)),
                }
            };

            rows.push(NativeSessionRow {
                agent: agent.clone(),
                native_id: summary.native_id.as_str().to_string(),
                workdir: summary.workdir.clone(),
                title: summary.title.clone(),
                started_at_ms: summary.started_at_ms,
                ended_at_ms: summary.ended_at_ms,
                event_count: summary.event_count,
                parent_native_id: summary
                    .parent_native_id
                    .as_ref()
                    .map(|parent| parent.as_str().to_string()),
                fidelity: summary.fidelity,
                id: id.encode(),
                blocked,
            });
        }
    }

    // Newest first, on whichever stamp the store carried. `i64::MIN` for a row with
    // neither, so an unsettled store sorts last rather than first.
    rows.sort_by_key(|row| {
        std::cmp::Reverse(row.ended_at_ms.or(row.started_at_ms).unwrap_or(i64::MIN))
    });

    let total = rows.len();
    let page: Vec<NativeSessionRow> = rows.into_iter().skip(offset).take(limit).collect();
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
/// this goes through the same `contain_workdir` the create path uses, which
/// canonicalizes once and uses **that** variable. A containment check paired with a path
/// it did not produce is not a bound, and that exact shape was two separate holes in this
/// repo a day apart (`docs/architecture.md` § Filesystem exposure).
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
/// grows and has no end to wait for, so it is spawned onto a task that outlives the
/// request — the same shape `links::LinkManager::spawn_drain` gives a live link, for the
/// same reason.
pub async fn adopt(
    State(state): State<AppState>,
    Json(req): Json<AdoptSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>)> {
    let environment = require_environment(&state, &req.environment).await?;
    let agent = req.agent.trim().to_string();

    if environment.as_str() != state.environment.descriptor().id {
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
                return Err(Error::InvalidRequest(format!("{code}: {message}")))
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
        let workdir = result
            .get("workdir")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| Error::LinkFailed("remote adopt returned no workdir".into()))?
            .to_string();
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
        return Ok((
            StatusCode::CREATED,
            Json(SessionResponse { session: stored }),
        ));
    }

    let native_raw = req.native_id.trim();
    if native_raw.is_empty() {
        return Err(Error::InvalidRequest(
            "Adopting a session needs the agent's own session id.".to_string(),
        ));
    }
    let native_id = NativeId::new(native_raw);
    let session_id = SessionId::new(
        environment,
        AgentName::parse(agent.clone())
            .map_err(|error| Error::InvalidRequest(error.to_string()))?,
        native_id.clone(),
    );

    // # Already-present is checked before the agent is, and the order is a decision
    //
    // Both are refusals, and this one is **more specific and cheaper**: it is a fact about
    // this exact request that no configuration changes. Checking the agent first means a
    // session Sylph already holds, on an agent whose CLI has since been uninstalled,
    // answers *"install that CLI"* — sending a reader to their `PATH` when the true answer
    // is "already done, open it". The reverse mistake does not exist: an agent that cannot
    // be read has no session to already hold.
    //
    // The primary key would refuse the insert regardless; checking first is what makes the
    // answer a `409` about this request rather than a database error two layers down.
    if state.store.get_session(&session_id).await?.is_some() {
        return Err(Error::SessionAlreadyPresent);
    }

    // One agent, and it must be installed: the store to read is on this machine.
    require_agents(state.adapters.as_ref(), Some(agent.as_str()))?;

    let adapter = state.adapters.for_agent(&agent).ok_or_else(|| {
        // Raced an uninstall between the check above and here.
        Error::InvalidRequest(format!("'{agent}' is not installed on this environment."))
    })?;

    // # Why a listing pass rather than a lookup
    //
    // The adapter contract has no "describe one session" call, and adding one would be the
    // per-session-metadata shape `Adapter::enumerate` exists to avoid. A filtered listing
    // answers the same question with one short header read per file, and it is the *same*
    // call the picker made — so the workdir this bounds is the one the picker showed.
    let summary = adapter
        .enumerate(&sylph_protocol::StoreFilter::default())
        .await
        .map_err(|error| {
            // Reading the store failed, which is not the same as the session being
            // absent: a `404` here would tell a user their id was wrong when the store
            // was unreadable.
            Error::Internal(format!("Could not read {agent}'s native store: {error:#}"))
        })?
        .into_iter()
        .find(|candidate| candidate.native_id == native_id)
        .ok_or_else(|| Error::NativeSessionNotFound(native_id.as_str().to_string()))?;

    // A session the store recorded no directory for. `422`: the id is real and nothing
    // the caller can change makes it adoptable, because a workdir is the `current_dir` of
    // a spawned agent and guessing one decides where arbitrary code runs.
    let recorded = summary
        .workdir
        .as_deref()
        .ok_or(Error::NativeSessionNotResumable)?;

    // The canonical path, produced *by* the containment check, and the one that gets
    // journalled. `400` when the directory is gone, `403` when it resolves outside the
    // roots — [`contain_workdir`] already draws that line for the create path.
    let workdir = contain_workdir(&state, recorded)?;

    // `stream` is unreachable here by protocol: no care recovers a timing that was never
    // written. Refused rather than silently downgraded, because a `stream` record would
    // promise a renderer per-delta timings the store does not hold.
    if summary.fidelity == sylph_protocol::Fidelity::Stream {
        return Err(Error::Internal(format!(
            "{agent}'s store reported `stream` fidelity for a session read from disk, \
             which a native store cannot hold"
        )));
    }

    let now = now_ms();
    let record = crate::models::Session {
        id: session_id.clone(),
        workdir,
        // The vendor's own title, or the client's override. The opening-ask fallback
        // needs the transcript, so it runs after ingestion — a title is a label, and
        // labelling a session before reading it would mean reading the store twice.
        title: req
            .title
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty())
            .or_else(|| summary.title.clone()),
        inject_identity: req.inject_identity,
        fidelity: summary.fidelity,
        flags_json: "{}".to_string(),
        status: "idle".to_string(),
        // Lineage is not a column on this row. It is a typed edge, written below once the
        // child exists (ADR-0001 § 2).
        // The store's own stamps where it kept them, so an adopted session sorts by when
        // the work happened rather than by when it was adopted.
        created_at_ms: summary.started_at_ms.unwrap_or(now),
        updated_at_ms: summary.ended_at_ms.or(summary.started_at_ms).unwrap_or(now),
    };

    let stored = match state.store.create_session(&record).await {
        Ok(stored) => stored,
        Err(crate::error::Error::Database(sqlx::Error::Database(db)))
            if db.is_unique_violation() =>
        {
            // Two adopts of one id, racing. The pre-check above lost.
            return Err(Error::SessionAlreadyPresent);
        }
        Err(error) => return Err(error),
    };

    record_adopted_lineage(
        &state,
        &session_id,
        summary.parent_native_id.as_ref(),
        summary.parent_relation,
    )
    .await;

    if req.watch {
        // Follows the store as it grows, so there is no end to await. Spawned rather than
        // joined, which is the same shape a live link's drain has: the response reports
        // the session exists, and the transcript fills in as the vendor writes it.
        let ingest = state.ingest.clone();
        let watched = session_id.clone();
        let stream = adapter.watch(&native_id).await.map_err(|error| {
            Error::Internal(format!(
                "Could not watch {agent}'s native session: {error:#}"
            ))
        })?;
        tokio::spawn(async move {
            match ingest.drain(&watched, stream).await {
                Ok(report) => tracing::info!(
                    "Watched {}: {} event(s) in {} turn(s), {} read error(s)",
                    watched,
                    report.events,
                    report.turns,
                    report.read_errors
                ),
                Err(error) => tracing::error!(
                    "Stopped watching {} because lossless native ingest failed: {error:#}",
                    watched
                ),
            }
        });
        return Ok((
            StatusCode::CREATED,
            Json(SessionResponse { session: stored }),
        ));
    }

    // One pass, awaited: a `201` means the transcript is readable, not that a task was
    // started. A client refetching history on the response would otherwise race it.
    let stream = adapter.adopt(&native_id).await.map_err(|error| {
        Error::Internal(format!(
            "Could not read {agent}'s native session: {error:#}"
        ))
    })?;
    let report = state
        .ingest
        .drain(&session_id, stream)
        .await
        .map_err(|error| {
            Error::Internal(format!(
                "Could not retain {agent}'s native session losslessly: {error:#}"
            ))
        })?;
    tracing::info!(
        "Adopted {}: {} event(s) in {} turn(s), {} read error(s)",
        session_id,
        report.events,
        report.turns,
        report.read_errors
    );

    // The opening-ask fallback, for the sessions no vendor titled. Runs on what the pass
    // recovered rather than on a second read of the store, and `set_session_title_if_empty`
    // is what keeps it from overwriting a title the vendor or the client gave.
    if let Some(opening) = &report.opening_status {
        let label = crate::db::labels::summarize_line(opening);
        if !label.is_empty() {
            state
                .store
                .set_session_title_if_empty(&session_id, &label)
                .await?;
        }
    }

    // Re-read, so the response carries the title the fallback may have just written
    // rather than the one the insert had.
    let session = state
        .store
        .get_session(&session_id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    Ok((StatusCode::CREATED, Json(SessionResponse { session })))
}
