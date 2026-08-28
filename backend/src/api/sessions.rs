//! Sessions: listing, reading, starting, adopting, and patching.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sylph_protocol::{EnvironmentId, SessionId};

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
/// A path segment is the encoded triple (`docs/api-design.md` § A session id in a
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
    session: Session,
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
        kind: block.kind.clone(),
        phase: block.phase.clone(),
        stream_id: block.stream_id.clone(),
        span: block.span.clone(),
        name: block.name.clone(),
        text: block.text.clone(),
        data: block.data.clone(),
        is_error: block.is_error,
        stability: "stable".to_string(),
        source: block.source.clone(),
        created_at_ms: block.created_at_ms,
        // The block already carries this; it was simply never published. Without it a
        // settled block reports only when it closed, so every duration computed from
        // durable history was a reconstruction from whatever the interior happened to
        // forward — exact for a chatty delegate, a lower bound for a quiet one, and
        // absent entirely for one that forwarded nothing.
        started_at_ms: Some(block.started_at_ms),
    }
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
/// **This starts an agent.** It does not reserve a row.
///
/// A session has no identity until an agent has assigned one
/// (`docs/architecture.md` § Identity), so there is nothing to create in advance and
/// the insert follows the spawn. [`crate::links::LinkManager::open_session`] owns that
/// ordering; this handler validates what a client asked for and hands it over.
///
/// What this deletes, recorded because the shape is easy to reinvent: the previous
/// version minted a `Uuid::new_v4()` here and wrote a row with the agent unstarted,
/// producing a class of session with no work in it. `DELETE /sessions/{id}` for
/// empties, the frontend probe that called it on unmount, and the window where a
/// session had no native id all go with it. A failed spawn now reports at the start
/// screen instead of leaving a dead row behind.
pub async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>)> {
    let environment = EnvironmentId::parse(req.environment.trim())
        .map_err(|error| Error::InvalidRequest(error.to_string()))?;

    // Refused before the spawn rather than after, so an unknown agent is a 400 about
    // the request instead of a failed process.
    // Read from the same place the link path resolves an adapter, so this gate cannot
    // refuse an agent a link could start. `AGENTS.md` records the inverse of that
    // mismatch costing a release: discovery listed an agent as usable while every link
    // to it failed naming a different root.
    let known = crate::adapters::available(&state.config.agent_roots());
    if !known.iter().any(|name| name == &req.agent) {
        return Err(Error::InvalidRequest(format!(
            "Unknown agent: {}. This build can run: {:?}. Availability is per \
             environment — see GET /api/v1/environments.",
            req.agent, known
        )));
    }

    // Existence is necessary and nowhere near sufficient: this path is where a process
    // gets spawned.
    let workdir = contain_workdir(&state, &req.workdir)?;

    let flags = req
        .flags
        .as_ref()
        .and_then(|value| value.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(key, value)| {
                    value
                        .as_str()
                        .map(|text| (key.clone(), text.to_string()))
                        .or_else(|| Some((key, value.to_string())))
                })
                .collect()
        })
        .unwrap_or_default();

    let session = state
        .links
        .open_session(crate::links::OpenSpec {
            environment,
            agent: req.agent,
            workdir,
            title: req.title.map(|title| title.trim().to_string()),
            inject_identity: req.inject_identity,
            flags,
            // Set only by a fork, which is a separate operation.
            parent_session: None,
        })
        .await
        // A typed error travels through `anyhow` and is recovered by **downcast, never
        // by matching the message text** — the same rule `AGENTS.md` states for adapter
        // refusal codes: the wording is the producer's and may be reworded, while the
        // type is declared contract. Without this, a duplicate triple would be reported
        // as `AGENT_START_FAILED`, which is a 502 blaming the agent for a 409 about the
        // request.
        .map_err(|error| match error.downcast::<Error>() {
            Ok(typed) => typed,
            Err(other) => Error::AgentStartFailed(other),
        })?;

    Ok((
        StatusCode::CREATED,
        Json(SessionResponse { session }),
    ))
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
        if !crate::adapters::available(&state.config.agent_roots())
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
            let schema = &link.identity().flags;
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

/// POST /api/v1/sessions/adopt
///
/// Bring in a session an agent already has, from that agent's own native store.
///
/// # Not yet implemented, and refused rather than faked
///
/// This needs [`sylph_protocol::Adapter::adopt`] and `enumerate` — the parse of a
/// vendor's native store — which is step 2 of `docs/roadmap.md` § Implementation order
/// and is being written concurrently. Every *other* part of the endpoint is decided and
/// recorded here, because the shape is what the next change needs:
///
/// - The workdir comes from the **store's own record**, not the request, and is bounded
///   exactly like a created one. That is not belt-and-braces: a machine's store holds
///   every session any agent ever ran there, so an unbounded adopt is a general way to
///   start an agent anywhere — find a session that ran in the target directory, adopt
///   it, send a turn. That the path came from the store rather than the request changes
///   who chose it, not what happens next.
/// - `fidelity` is what the vendor's store actually holds — `blocks` or `messages`,
///   **never `stream`**. No care recovers a timing that was never written.
/// - `injectIdentity` defaults to false, because the transcript so far was produced
///   without it.
/// - The title falls back to the opening ask through [`crate::db::labels`], whose rule
///   skips the agent's injected envelopes.
///
/// Answering `501` is deliberate: a stub that created a row with an empty transcript
/// would be a session claiming to hold history it never read, and a journal makes that
/// permanent.
pub async fn adopt(
    State(_state): State<AppState>,
    Json(_req): Json<AdoptSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>)> {
    Err(Error::NotImplemented(
        "Adopting a native session needs the vendor-store parsers, which land with \
         `crates/adapters`. Nothing is recorded rather than recording a session with \
         no transcript."
            .to_string(),
    ))
}
