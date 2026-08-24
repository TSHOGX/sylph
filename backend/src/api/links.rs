//! Endpoints for a session's live agent: its capabilities, and the questions it
//! is waiting on.
//!
//! These are the surfaces v1 could not have had. What a session can do is read
//! from a live link rather than from a static descriptor, and an approval is a
//! resource a client resolves rather than a message it happened to witness.

use axum::extract::{Path, State};
use axum::Json;
use tokio_stream::StreamExt;

use crate::error::{Error, Result};
use crate::links::LinkState;
use crate::models::{PendingRequest, ResolveRequest};
use crate::AppState;

/// What this session's agent can do right now.
///
/// Establishes a link if there is none, because the answer is only knowable from
/// a live agent — the project's command list depends on files the CLI reads at
/// startup. A caller that merely wants to know whether a link exists reads the
/// session's `link` field instead of paying for one here.
pub async fn capabilities(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<LinkState>> {
    require_session(&state, &session_id).await?;

    if let Err(error) = state.links.ensure(&session_id).await {
        // A missing adapter or a refusing agent is a real failure, but it is the
        // agent's failure rather than the request's: the session is still valid
        // and its history still readable, so this reports detached with a reason
        // rather than a 500 that would make the whole session look broken.
        tracing::warn!("Could not establish a link for {}: {:#}", session_id, error);
    }

    Ok(Json(state.links.state(&session_id).await))
}

/// Establish a link eagerly.
///
/// Worth having as its own operation because establishment is slow — an agent CLI
/// loads a project — and a UI that knows a user is about to type can pay that
/// cost before they finish rather than after they send.
pub async fn attach(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<LinkState>> {
    require_session(&state, &session_id).await?;

    state
        .links
        .ensure(&session_id)
        .await
        .map_err(|error| Error::LinkFailed(format!("{:#}", error)))?;

    Ok(Json(state.links.state(&session_id).await))
}

/// Tear a link down without ending the session.
pub async fn detach(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<LinkState>> {
    require_session(&state, &session_id).await?;
    state.links.detach(&session_id, "client").await;
    Ok(Json(state.links.state(&session_id).await))
}

/// What this session is waiting on.
///
/// Read from the journal rather than from the link, so it answers after a client
/// reconnects, from a different frontend, and for a session whose link has since
/// died — the last of which is exactly when a user needs to know why nothing is
/// happening.
pub async fn list_requests(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<PendingRequest>>> {
    require_session(&state, &session_id).await?;
    Ok(Json(state.store.open_requests(&session_id).await?))
}

/// Answer a request an agent raised.
pub async fn resolve(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
    Json(body): Json<ResolveRequest>,
) -> Result<Json<PendingRequest>> {
    let request = state
        .store
        .get_request(&request_id)
        .await?
        .ok_or(Error::RequestNotFound)?;

    // The option has to be one the agent offered. A client inventing one would
    // reach the adapter as an unrecognized string, and an adapter's handling of
    // that is undefined — for an approval, "unrecognized" must never be able to
    // mean "allow".
    let offered = request.options();
    let known = offered
        .as_array()
        .map(|options| {
            options.iter().any(|option| {
                option.get("id").and_then(|id| id.as_str()) == Some(body.option.as_str())
            })
        })
        .unwrap_or(false);
    if !known {
        return Err(Error::InvalidRequest(format!(
            "'{}' is not an option this request offered",
            body.option
        )));
    }

    let input_json = body.input.as_ref().map(|value| value.to_string());

    // Claimed in one statement, so two clients answering together produce one
    // winner rather than two forwarded answers.
    let resolved = state
        .store
        .resolve_request(&request_id, &body.option, input_json.as_deref(), None)
        .await?
        .ok_or(Error::RequestAlreadyResolved)?;

    // Forwarded only after the claim succeeds. Sending first would let the loser
    // of a race also reach the adapter, and for an approval the second answer
    // would overwrite the first's decision.
    match state.links.get(&request.session_id).await {
        Some(link) => {
            link.resolve(&request.adapter_request_id, &body.option, body.input)
                .await
                .map_err(|error| Error::LinkFailed(format!("{:#}", error)))?;
        }
        None => {
            // The row is already marked resolved, which is correct: the user did
            // answer. But nothing is waiting for it, and saying so is better than
            // a success that silently went nowhere.
            return Err(Error::LinkGone);
        }
    }

    Ok(Json(resolved))
}

/// Interrupt the running turn, keeping the session alive.
///
/// Deliberately not `stop`. Stop signals the process group and ends the agent;
/// interrupt asks the agent to abandon this turn and stay loaded, so the next
/// message continues the same conversation. They are different intentions — "that
/// is not what I meant" against "stop everything" — and offering only one means
/// the other is unavailable.
pub async fn interrupt(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require_session(&state, &session_id).await?;

    let link = state.links.get(&session_id).await.ok_or(Error::LinkGone)?;

    link.interrupt()
        .await
        // The link refuses locally when the adapter never declared interrupt, so
        // this is a capability answer rather than a transport failure.
        .map_err(|error| Error::Unsupported(format!("{:#}", error)))?;

    Ok(Json(serde_json::json!({"interrupted": true})))
}

/// Add text to a turn already in flight.
pub async fn steer(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(body): Json<SteerRequest>,
) -> Result<Json<serde_json::Value>> {
    require_session(&state, &session_id).await?;

    if body.text.trim().is_empty() {
        return Err(Error::InvalidRequest("Steering text cannot be empty".into()));
    }

    let link = state.links.get(&session_id).await.ok_or(Error::LinkGone)?;

    link.steer(body.text)
        .await
        .map_err(|error| Error::Unsupported(format!("{:#}", error)))?;

    Ok(Json(serde_json::json!({"steered": true})))
}

#[derive(serde::Deserialize)]
pub struct SteerRequest {
    pub text: String,
}

/// Session-scoped facts, as SSE.
///
/// Separate from the turn stream because links, capability changes, and requests
/// are properties of a session: they happen with no turn running, and a client
/// watching only turns would miss an approval raised while it was idle.
///
/// Carries no replay. These are notifications that state changed, and the state
/// itself is a resource — `/requests` and `/capabilities` — so a client that
/// reconnects re-reads rather than replaying. That is what keeps a missed frame
/// from stranding an approval.
pub async fn events(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<impl axum::response::IntoResponse> {
    require_session(&state, &session_id).await?;

    let live = state.hub.attach_session(&session_id).await;
    let stream = tokio_stream::wrappers::BroadcastStream::new(live)
        .filter_map(|result| result.ok())
        .map(|event| {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Ok::<axum::response::sse::Event, std::convert::Infallible>(
                axum::response::sse::Event::default().event(&event.kind).data(data),
            )
        });

    Ok((
        // `no-transform` for the reason recorded in AGENTS.md: a proxy that gzips
        // an SSE body buffers it invisibly, and the symptom reads as a rendering
        // bug rather than a transport one.
        [
            ("Cache-Control", "no-cache, no-store, no-transform"),
            ("X-Accel-Buffering", "no"),
        ],
        axum::response::sse::Sse::new(stream)
            .keep_alive(axum::response::sse::KeepAlive::default()),
    ))
}

/// 404 for an unknown session before anything expensive happens.
///
/// Without this, asking for capabilities on a mistyped id would try to establish
/// a link, fail on a missing session deeper in, and report that as an agent
/// problem rather than as a bad id.
async fn require_session(state: &AppState, session_id: &str) -> Result<()> {
    state
        .store
        .get_session(session_id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    Ok(())
}
