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
        //
        // The reason travels in the body. Logging it and returning a bare
        // `attached: false` was the defect: a client could not tell a failure from
        // an agent that simply declares no commands, so the only honest thing it
        // could render was nothing at all.
        tracing::warn!("Could not establish a link for {}: {:#}", session_id, error);
        return Ok(Json(state.links.state_after_failure(&session_id, &error).await));
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

    // And when the request declared a form, the answer has to fit it. Checked here,
    // before the claim below, for a reason the ordering makes load-bearing: a
    // malformed answer that reached the UPDATE would consume the single-winner claim
    // and mark the request resolved, leaving the agent blocked on a question nobody
    // can answer any more. Refusing first leaves it open and answerable.
    //
    // This is the same rule that governs `option` above and a `set_config` value: the
    // client renders whatever an agent it has never seen reported, so the server is
    // what keeps a malformed answer from reaching a vendor CLI whose handling of one
    // is undefined.
    if let Some(form) = request.form() {
        // Only when the answer is being submitted. Declining carries no values, and
        // requiring a complete form in order to say "don't answer" would make a
        // required field into a trap the user cannot leave.
        let declining = body.option.starts_with("deny") || body.option == "cancel";
        if !declining {
            form.validate(body.input.as_ref())
                .map_err(Error::InvalidRequest)?;
        }
    }

    // Claimed in one statement, so two clients answering together produce one
    // winner rather than two forwarded answers.
    let resolved = state
        .store
        .resolve_request(&request_id, &body.option, input_json.as_deref(), None)
        .await?
        .ok_or(Error::RequestAlreadyResolved)?;

    // Announced before the forward, and deliberately so. The claim above is what makes
    // this request answered — the row is committed and no second answer can win — so
    // every other client may stop showing it from this moment, whether or not the
    // adapter is still there to hear the answer. Announcing after the forward would
    // tie a *display* concern to a *delivery* one, and the `LinkGone` path below
    // returns early: a request answered on a phone against a dead link would then stay
    // on screen on every other client, which is the bug this fixes, surviving in the
    // one case where the user most needs to be told what happened.
    //
    // This is the counterpart of `absorb_request`'s announcement. Arrival was published
    // and departure was not, so a client's view of what a session is blocked on could
    // only ever grow; it shrank on a 20-second poll that a backgrounded tab does not
    // run, which is why answering on one device left the dialog standing on another
    // until a manual reload.
    state
        .links
        .announce_request(&request.session_id, &resolved)
        .await;

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
///
/// `turnId` is a precondition rather than a routing hint. A steer that races a turn
/// boundary would otherwise be delivered into the *next* turn — the request
/// succeeds, nothing errors, and the text lands in the wrong conversation. Naming
/// the turn the client believed was running turns that into a refusal.
pub async fn steer(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(body): Json<SteerRequest>,
) -> Result<Json<serde_json::Value>> {
    require_session(&state, &session_id).await?;

    if body.text.trim().is_empty() {
        return Err(Error::InvalidRequest("Steering text cannot be empty".into()));
    }

    // Checked here as well as by the adapter: the server knows which turn is
    // running, so a stale client can be told so without waking the agent.
    let running = state.store.running_turn_id(&session_id).await?;
    if let Some(expected) = body.turn_id.as_deref() {
        if running.as_deref() != Some(expected) {
            return Err(Error::TurnMoved {
                expected: expected.to_string(),
                running: running.clone(),
            });
        }
    }

    let link = state.links.get(&session_id).await.ok_or(Error::LinkGone)?;

    link.steer(body.text, running)
        .await
        .map_err(|error| Error::Unsupported(format!("{:#}", error)))?;

    Ok(Json(serde_json::json!({"steered": true})))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerRequest {
    pub text: String,
    /// The turn the client believes is running. Optional, because a client with no
    /// turn in view is steering "whatever is running", which is a coherent ask.
    #[serde(default)]
    pub turn_id: Option<String>,
}

/// Set one typed config option on a live session.
///
/// The response is the refreshed capability surface rather than an acknowledgement,
/// because setting one option can change another's choices — `pi`'s thinking levels
/// depend on the selected model, and `claude` carries per-model effort levels. A
/// client that assumed its own optimistic value would drift from the agent.
pub async fn set_config(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(body): Json<SetConfigRequest>,
) -> Result<Json<LinkState>> {
    require_session(&state, &session_id).await?;

    if body.config_id.trim().is_empty() {
        return Err(Error::InvalidRequest("A config id is required".into()));
    }

    let link = state.links.ensure(&session_id).await.map_err(|error| {
        Error::LinkFailed(format!("{:#}", error))
    })?;

    // The option has to be one the agent offered, for the same reason a request
    // option is validated: an id the adapter does not recognize reaches the vendor
    // CLI as an unknown setting, and what that does is undefined.
    let surface = link.capabilities().await;
    let Some(option) = surface
        .config
        .iter()
        .find(|option| option.id == body.config_id)
    else {
        return Err(Error::InvalidRequest(format!(
            "'{}' is not a config option this session offers",
            body.config_id
        )));
    };

    // And the *value* has to be one it enumerated. Validating only the id was the
    // asymmetry worth closing: the approval path has always checked its option
    // against what was offered, while a config value went through untouched. A
    // mistyped one then reached the vendor CLI, which is exactly how `pi` came to
    // answer `Model not found: undefined/undefined` — an error whose wording named
    // fields no user had ever seen, three layers from where it could be read.
    if !option.accepts(&body.value) {
        return Err(Error::InvalidRequest(format!(
            "that is not a value '{}' offers",
            body.config_id
        )));
    }

    // A refusal is classified by the adapter's own code rather than flattened to one
    // status. The distinction is the difference between "this agent cannot do that"
    // and "that value is not allowed right now": the first justifies disabling the
    // control, the second must leave it usable, since the other five values may well
    // be fine. `claude` reports exactly this shape when a permission mode is refused
    // for a reason the session cannot change, and reporting it as unsupported sent the
    // last investigation to the picker.
    link.set_config(&body.config_id, body.value)
        .await
        .map_err(|error| match error.downcast_ref::<sylph_protocol::AdapterError>() {
            Some(refusal) if refusal.code == sylph_protocol::RefusalCode::InvalidValue => {
                Error::InvalidRequest(refusal.message.clone())
            }
            _ => Error::Unsupported(format!("{:#}", error)),
        })?;

    // Published so every other attached client sees the change rather than only the
    // one that made it — the same reason a capability update is pushed at all.
    state.links.announce_capabilities(&session_id).await;

    Ok(Json(state.links.state(&session_id).await))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConfigRequest {
    pub config_id: String,
    pub value: serde_json::Value,
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
