//! The live `claude` session: one held process, and turns both ways.
//!
//! The session: the held process, its control protocol, and its turn loop
//! (roughly :250-700, :1783-1945). [`crate::process::Child`] owns the process group, the
//! single stdout reader, stderr retention and the shutdown escalation, so none of that is
//! here. What is here is everything specific to this vendor's *control* channel.
//!
//! # What this vendor's [`crate::process::ReplyId`] has to say, and why both halves matter
//!
//! Printed from real frames on claude 2.1.251, and then confirmed against the vendor's own
//! implementation, which is the better evidence: `injectControlResponse` reads
//! `t.response?.request_id` — the nested path — and on `subtype === "error"` calls
//! `o.reject(Error(t.response.error))` without reading a payload. So the CLI implements
//! exactly the two shapes measured here, rather than a sample happening to look this way.
//!
//! A reply is `{"type":"control_response","response":{"subtype":"success","request_id":"…",…}}`
//! — the id is **one level down** — while a request the *vendor* raised is
//! `{"type":"control_request","request_id":"…","request":{…}}`, with its id at the top level.
//! So `reply_id` does three things and each prevents a distinct silent failure:
//!
//! - **It tests `type` first.** `can_use_tool` and `AskUserQuestion` arrive as
//!   `control_request` on this same channel. A predicate reading only an id would route an
//!   approval into whichever waiter held it: the request never reaches the event stream, so
//!   nobody is asked and the turn waits on a human forever, while the caller receives an
//!   approval as its reply. Both directions silent.
//! - **It reads the id from inside `response`.** Any top-level lookup finds nothing here,
//!   so *no* control on this vendor would correlate and every one would time out after 30 s
//!   while the CLI answered correctly.
//! - **It requires our own prefix.** This channel carries frames from id spaces this session
//!   does not own. Unlike the two above it is **defence rather than a measured collision**,
//!   and `REQUEST_PREFIX` says exactly how far the evidence goes.
//!
//! # Reading a control reply: the discriminant comes first
//!
//! A success is `{"subtype":"success","request_id":…,"response":{…}}` and a refusal is
//! `{"subtype":"error","request_id":…,"error":"…"}` — **no `response` member at all**.
//! Reading `response.response or {}` mapped every refusal onto the same empty value a
//! successful no-payload control returns, and the cost was the reported bug: selecting
//! `bypassPermissions` was refused by the CLI, recorded as applied, answered 200, and
//! rendered as the current mode while every tool call went on prompting. `Control::send`
//! reads `subtype` before it reaches for a payload, so `{}` is reachable from exactly one
//! outcome.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use futures::StreamExt;
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};

use sylph_protocol::{
    AdapterError, CapabilityFacets, ConfigOption, EventStream, NativeId, RequestedNativeId,
    Resolution, RunSpec, Session, SessionCapabilities, SessionMessage, TurnInput,
};

use super::requests::{answer_payload, ask_from, AskKind};
use super::surface::{capabilities_from, config_from, models_from};
use super::{Correlation, SETTABLE_PERMISSION_MODES, SUPPORTED_DIALOG_KINDS};
use crate::now_ms;
use crate::process::{Child, Line, Spawn};

/// The vendor refused a control it understood.
///
/// Its own type because a refusal is an **outcome**, not a transport failure, and the two
/// need different handling: a refusal carries the vendor's own reason and is worth showing
/// a user, while a dead pipe is not. It is recovered by `downcast_ref` at the
/// [`Session::set_config`] boundary and turned into [`AdapterError::invalid_value`],
/// because the vendor's sentence names the *precondition* — "because the session was not
/// launched with --dangerously-skip-permissions" — which is exactly the diagnosis a
/// generic adapter error destroys.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct VendorRefused(pub String);

/// This session's half of the control channel.
///
/// Separate from [`ClaudeSession`] so the pump task can answer the vendor and re-read the
/// surface while holding nothing else.
struct Control {
    child: Child,
}

/// The prefix on every id this side mints.
///
/// The wire id is `sylph-<n>` where `<n>` is [`Child::next_request_id`]'s decimal, which is
/// also the key [`Child::request`] correlates on — so [`reply_id`] strips this and hands back
/// the decimal.
///
/// **Unlike the other two conditions in [`reply_id`], this one is defence rather than a
/// measured collision, and it is worth saying which.**
///
/// That this channel carries frames from id spaces this session does not own is a vendor
/// fact, and two mechanisms in `injectControlResponse`'s object show it:
///
/// - **`forwarded` is a flag on a pending request.** `asksOurHuman(t)` is
///   `!t.forwarded && …`, so the CLI itself distinguishes a request it owns from one
///   forwarded from elsewhere.
/// - **A reply can outlive the process it was meant for.** `ignoresResponseAtShutdown`
///   logs `Leaving control_response for request_id=… to the next process — this one is
///   shutting down and settles no question` and counts it in `repliesLeftForNextProcess`.
///
/// What is *not* established is whether such an id could ever equal the decimal spelling of
/// an outstanding id of ours. The only id source identifiable in the binary is
/// `crypto.randomUUID()`, which cannot — but a minified bundle is not evidence about every
/// source. So the prefix makes the question moot for four characters on the wire, and the
/// failure it forecloses is silent both ways: a foreign reply consuming a waiter, so our
/// caller reads someone else's answer while the real reply is dropped.
///
/// An earlier version of this comment cited the CLI's "not an answer to it" log line here.
/// **That was the wrong mechanism** — it is in `ignoresUnsettlingDeviceResponse` and is about
/// a *device* request whose reply does not settle it. Recorded because a misattributed
/// citation is worse than an uncited assertion: it reads as checked.
const REQUEST_PREFIX: &str = "sylph-";

/// This vendor's [`crate::process::ReplyId`]: the id off a frame that answers *us*.
///
/// Free-standing rather than inline at the spawn so it is directly testable — the whole point
/// of the closure form is that direction is a vendor fact, and a vendor fact deserves a test
/// rather than a comment.
fn reply_id(frame: &Value) -> Option<String> {
    // **First, and this is the measured one.** A request the *vendor* raised carries
    // `request_id` too, at the top level: printed from 2.1.251, a reply is
    // `{"type":"control_response","response":{"request_id":…}}` and an ask is
    // `{"type":"control_request","request_id":…,"request":{…}}`. Reading an id without this
    // routes an approval into whichever waiter holds it — nobody is asked, the turn waits on
    // a human forever, and the caller receives an approval as its reply. Both silent.
    if frame.get("type").and_then(Value::as_str) != Some("control_response") {
        return None;
    }
    frame
        .get("response")
        // Inside `response`, which is where this vendor puts it. A top-level lookup finds
        // nothing, so *no* control would correlate and every one would time out after 30 s
        // while the CLI answered correctly.
        .and_then(|body| body.get("request_id"))
        // The shared read, so a string and a number are both understood here as everywhere
        // else. A number reaching this field is somebody else's id, which the prefix rejects.
        .and_then(crate::process::reply_id)
        .and_then(|id| id.strip_prefix(REQUEST_PREFIX).map(str::to_string))
}

impl Control {
    /// Send one `control_request` and read its reply.
    ///
    /// Correlation is [`Child::request`]'s, driven by [`reply_id`] — one pending map for the
    /// process rather than a second one here. What stays this module's job is **reading the
    /// reply**, which the transport cannot do: a refusal is still a `control_response` and
    /// still correlates, so the discriminant is an outcome rather than a routing question.
    ///
    /// Returns the reply's `response` member on success — `{}` when the control carries no
    /// payload, which several do (`apply_flag_settings` answers with no `response` at all,
    /// measured) — and [`VendorRefused`] when the vendor answered `subtype: "error"`.
    async fn send(&self, subtype: &str, fields: Value) -> Result<Value> {
        let mut request = Map::new();
        request.insert("subtype".to_string(), Value::String(subtype.to_string()));
        if let Value::Object(extra) = fields {
            for (key, value) in extra {
                request.insert(key, value);
            }
        }
        let request = Value::Object(request);

        // The reply arrives as the whole `response` **envelope**, because that is the object
        // [`reply_id`] found the id inside. Unwrapping to its `response` member is below,
        // after the discriminant.
        let envelope = self
            .child
            .request(|id| {
                json!({
                    "type": "control_request",
                    "request_id": format!("{REQUEST_PREFIX}{id}"),
                    "request": request,
                })
            })
            .await
            // **This is the only place the control is named**, and it carries the cause
            // alongside rather than wrapping it: `anyhow`'s single-line `to_string()` shows
            // only the outermost error, so `with_context` would name the control and hide
            // *why* — the half that says whether the vendor is slow, gone, or was never
            // asked. `Child::request` supplies that half, including the exited case and its
            // stderr tail (`8087085`), so this adapter checks `has_ended` nowhere: a second
            // guard here would be a second answer to one question, and it was measurably
            // dead — removing it changed no test.
            .map_err(|error| anyhow!("claude did not answer {subtype}: {error}"))?;
        let reply = envelope.get("response").cloned().unwrap_or(envelope);

        // **The discriminant, before any reach for a payload.** See this module's header:
        // an error envelope has no `response` member, so a reader that unwrapped first
        // converted every refusal into the empty value a successful no-payload control
        // returns.
        if reply.get("subtype").and_then(Value::as_str) == Some("error") {
            return Err(anyhow::Error::new(VendorRefused(
                reply
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("claude refused the request")
                    .to_string(),
            )));
        }
        Ok(reply
            .get("response")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())))
    }

    /// Answer one of the vendor's own requests.
    ///
    /// `subtype` is `success` for an answer and `error` for a subtype this adapter cannot
    /// render — the vendor is answered either way, because a parked request blocks the turn
    /// until its deadline.
    ///
    /// # Three guards the CLI runs on what arrives here, read from its own source
    ///
    /// `injectControlResponse` drops a reply that fails any of these, and a dropped reply
    /// leaves the request parked — so each one is a way for a correct-looking answer to
    /// vanish. All three shape what this function may send:
    ///
    /// - **`hasCanUseToolNameMismatch` is opt-in, not required.** It delegates to a helper
    ///   opening `if (typeof e !== "string") return false`, so an **absent** `toolName` is no
    ///   mismatch and the reply is accepted; only a *wrong* one is dropped, with
    ///   `toolName '…' does not match pending '…'`. So this sends no `toolName`, which is
    ///   both safe and the safer of the two: echoing it would add a field that has to stay
    ///   equal to the vendor's pending name forever, and the failure if it ever drifted is an
    ///   approval that vanishes. The Python reference omits it too.
    /// - **`ignoresErrorShapedDialogResponse` means an error may never answer a dialog.** An
    ///   `error`-shaped reply to a parked `request_user_dialog` is ignored and *the dialog
    ///   stays parked* — "not a human choice". This is why
    ///   [`super::requests::ask_from`] answers every `dialog_kind` including an unrecognized
    ///   one, rather than letting it fall through to the `error` path below: a coarse
    ///   accept/reject is not merely better than nothing, it is the only thing the vendor
    ///   will accept. The `error` path is reachable only for a subtype that is neither
    ///   `can_use_tool` nor `request_user_dialog`, where the guard does not apply.
    /// - **`ignoresResponseAtShutdown` can leave our reply for the next process.** Nothing to
    ///   do differently, and worth knowing when a resolution appears to have been ignored:
    ///   the CLI may have been shutting down, and it says so in its own log.
    async fn reply(&self, vendor_id: &str, subtype: &str, body: Value) -> Result<()> {
        let mut response = Map::new();
        response.insert("subtype".to_string(), Value::String(subtype.to_string()));
        response.insert(
            "request_id".to_string(),
            Value::String(vendor_id.to_string()),
        );
        match subtype {
            "error" => {
                response.insert("error".to_string(), body);
            }
            _ => {
                response.insert("response".to_string(), body);
            }
        }
        self.child
            .send(json!({"type": "control_response", "response": Value::Object(response)}))
            .await
    }
}

/// What the pump learns and the session's methods need, and vice versa.
#[derive(Default)]
struct Shared {
    /// Native history identity, assigned by Claude on the first real turn when the
    /// process was launched without `--session-id`.
    native_id: std::sync::RwLock<Option<NativeId>>,
    /// Pending vendor requests → the response shape each expects.
    ///
    /// **Remembered from the request, never inferred from the answer.** Sending a dialog's
    /// shape to a permission is not a validation error the CLI reports: it parks the
    /// request and the turn stalls until its deadline, which reads as the agent hanging.
    asks: Mutex<HashMap<String, AskKind>>,

    /// The model the **vendor** last said it is running.
    ///
    /// Arrives on `system/init` and nowhere else. *Measured on 2.1.251:* the `initialize`
    /// reply carries no current model — not on open, and not after a successful
    /// `set_model` either — so this is the only vendor source. Until it arrives,
    /// `pending_model` is used solely to keep a validated picker selection visible.
    observed_model: Mutex<Option<String>>,

    /// A model selected through Sylph's config control, but not yet echoed by
    /// Claude in a `system/init` notification.
    ///
    /// Claude's `set_model` acknowledgement has no payload and `initialize`
    /// does not expose the current model.  Keeping this adapter-local pending
    /// value lets a successful, catalogue-validated selection be reflected in
    /// the refreshed surface immediately.  The next vendor `system/init` is
    /// authoritative and clears this value before rebuilding capabilities.
    pending_model: Mutex<Option<String>>,
    /// Stops the bundle follower when the live session closes.
    closed: std::sync::atomic::AtomicBool,
}

/// A live `claude` session.
pub struct ClaudeSession {
    control: Arc<Control>,
    shared: Arc<Shared>,
    /// Yielded **once** by [`Session::events`]. A second call is a bug rather than a second
    /// subscription: two consumers of one vendor's stdio would each see half the turn.
    stream: Mutex<Option<mpsc::Receiver<Result<SessionMessage>>>>,
    /// The identity preamble's temporary file, removed on [`Session::close`].
    ///
    /// Held rather than deleted after the spawn: `--append-system-prompt-file` is read by
    /// the CLI when it starts, and on a slow start deleting it immediately is a race whose
    /// only symptom is a session with no identity.
    preamble: Option<PathBuf>,
}

/// Epoch milliseconds now.
///
/// `claude`'s `stream_event` envelopes carry **no timestamp at all** — printed and checked
/// — so the adapter stamps arrival. [`Correlation::translate`] takes it as an argument
/// rather than calling this, which is what keeps the projection a pure function.
/// Read the vendor's stdout forever, routing each line to the one thing that wants it.
///
/// Three destinations, and the split is what keeps [`super::translate`] process-free:
/// a `control_response` wakes its caller and **leaves the stream** (a reply is not an
/// event); a `control_request` becomes a [`sylph_protocol::Request`]; everything else is
/// the projection.
async fn pump(
    mut lines: mpsc::Receiver<Line>,
    out: mpsc::Sender<Result<SessionMessage>>,
    control: Arc<Control>,
    shared: Arc<Shared>,
) {
    let mut correlation = Correlation::new();

    while let Some(line) = lines.recv().await {
        let payload = match line {
            Ok(payload) => payload,
            Err(error) => {
                // Reported, not fatal — a vendor may print a banner, and one malformed line
                // must not fail a turn. `Child` already guarantees the stream continues.
                if out.send(Err(error)).await.is_err() {
                    return;
                }
                continue;
            }
        };

        match payload.get("type").and_then(Value::as_str) {
            // **Anything reaching here is not ours.** A reply to something this side sent was
            // taken by `Child`'s read loop and never entered this stream — that is what
            // [`reply_id`] is for. What is left is a `control_response` the CLI *forwarded*,
            // for a request another client issued; its own logs describe exactly that ("not
            // an answer to it"). Named and dropped rather than passed to `translate`, which
            // would ignore it anyway: an explicit branch is what says the case was
            // considered, where a fall-through reads as an oversight.
            Some("control_response") => {
                debug!(
                    request_id = payload
                        .get("response")
                        .and_then(|body| body.get("request_id"))
                        .and_then(|id| id.as_str())
                        .unwrap_or("?"),
                    "a control_response this session did not ask for"
                );
            }
            Some("control_request") => {
                let Some(vendor_id) = payload
                    .get("request_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    continue;
                };
                match ask_from(&payload) {
                    Some(ask) => {
                        shared.asks.lock().await.insert(vendor_id, ask.kind);
                        if out
                            .send(Ok(SessionMessage::Request(ask.request)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    None => {
                        // Answered rather than ignored, so claude is never left blocking on
                        // a subtype this adapter does not model. An `error` here is the
                        // vendor's own vocabulary for "the host cannot do this".
                        let subtype = payload
                            .get("request")
                            .and_then(|request| request.get("subtype"))
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        warn!(subtype, "unhandled vendor request subtype");
                        let _ = control
                            .reply(
                                &vendor_id,
                                "error",
                                Value::String(format!(
                                    "'{subtype}' is not supported by the Sylph adapter"
                                )),
                            )
                            .await;
                    }
                }
            }
            _ => {
                if let Some(id) = payload
                    .get("session_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                {
                    if let Ok(mut native) = shared.native_id.write() {
                        *native = Some(NativeId::new(id));
                    }
                }
                let emission = correlation.translate(&payload, now_ms());
                let observed_model = emission.observed_model;
                if let Some(ref model) = observed_model {
                    *shared.observed_model.lock().await = Some(model.clone());
                    // A real vendor report supersedes a locally pending set.
                    *shared.pending_model.lock().await = None;
                }
                if emission.refresh_capabilities || observed_model.is_some() {
                    // **Spawned, never awaited here.** This task is the only reader of the
                    // reply it is about to wait for, so awaiting inline would deadlock the
                    // whole session on its own `initialize`. `commands_changed` carries only
                    // that *something* changed, so the surface has to be re-read rather than
                    // reconstructed from the notification.
                    let control = control.clone();
                    let shared = shared.clone();
                    let out = out.clone();
                    tokio::spawn(async move {
                        match refreshed_surface(&control, &shared).await {
                            Ok(surface) => {
                                let facets = CapabilityFacets {
                                    commands: Some(surface.commands),
                                    subagents: Some(surface.subagents),
                                    config: Some(surface.config),
                                    permission_mode: surface.permission_mode,
                                    context_window: surface.context_window,
                                };
                                let _ = out.send(Ok(SessionMessage::Capabilities(facets))).await;
                            }
                            // Reported and not fatal: a session whose command list went
                            // stale is worse than one that could not refresh it, and the
                            // previous surface is still the last thing the vendor said.
                            Err(error) => warn!(%error, "failed to refresh claude's surface"),
                        }
                    });
                }
                for message in emission.messages {
                    if out.send(Ok(message)).await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    // stdout closed: the vendor is gone. **The stream simply ends**, which is what a
    // `Stream` already expresses — there is no exit code an in-process adapter can report,
    // and `SessionMessage` deliberately has no `Closed` variant.
    //
    // But a turn still open when that happens must not end in silence: a turn with no
    // content and no error says the same thing whether the model failed, the agent chose
    // not to answer, or the process died. So the one case that needs explaining gets an
    // `Err` before the end, which is where `Adapter::run`'s contract puts a failure that
    // needs explaining.
    shared
        .closed
        .store(true, std::sync::atomic::Ordering::Release);
    if correlation.turn_is_open() {
        let _ = out
            .send(Err(control
                .child
                .error("claude exited while a turn was still running")
                .await))
            .await;
    }
}

/// Follow Claude's on-disk bundle while the live stdout pump is running.
///
/// The root transcript is intentionally skipped by `watch_live`: stdout already carries
/// the root at stream fidelity. The follower waits for `system/init` to reveal the native
/// id (for `New` sessions), retries a bundle that has not been flushed yet, and then keeps
/// forwarding newly-created delegate/journal files until the session closes.
async fn bundle_pump(out: mpsc::Sender<Result<SessionMessage>>, shared: Arc<Shared>) {
    let mut current: Option<NativeId> = None;
    let mut stream: Option<EventStream> = None;

    loop {
        if shared.closed.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }

        let native = shared.native_id.read().ok().and_then(|id| id.clone());
        if native != current {
            current = native.clone();
            stream = native.and_then(|id| super::native::watch_live(&id).ok());
        }

        if let Some(bundle) = stream.as_mut() {
            tokio::select! {
                item = bundle.next() => {
                    match item {
                        Some(Ok(message)) => {
                            if out.send(Ok(message)).await.is_err() {
                                return;
                            }
                        }
                        Some(Err(error)) => {
                            if out.send(Err(error)).await.is_err() {
                                return;
                            }
                        }
                        None => {
                            // `watch_live` normally stays open for the session's lifetime;
                            // a transient file error should not disable the reader forever.
                            stream = None;
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

/// Re-read the whole surface from the vendor.
///
/// **An `initialize` round trip every time, deliberately.** The alternative is caching the
/// first reply and rebuilding from it, which is what the Python did — and it makes the
/// adapter answer from its own memory, so anything that changed the session by another
/// route (the agent's own `/model`, a plugin loading, a mode the CLI normalized) is
/// invisible until something happens to invalidate the cache. `docs/session-protocol.md`
/// § Three rules: read current values from the vendor, not from what you last set. The one
/// exception is Claude's model control: its acknowledgement and initialize response both
/// omit the current model, so a catalogue-validated pending selection is exposed until the
/// next `system/init` report arrives.
///
/// `supportedDialogKinds` is **not** re-sent. Per the CLI's own schema the declaration is
/// first-attached-client-wins and later initializes do not change it, so re-sending would
/// be a field that reads as effective and is not.
async fn refreshed_surface(control: &Control, shared: &Shared) -> Result<SessionCapabilities> {
    let initialize = control.send("initialize", json!({})).await?;
    // A pending local selection is preferred until the vendor reports the
    // resolved native model.  This is only an adapter presentation hint; the
    // vendor's later `system/init` always wins.
    let pending = shared.pending_model.lock().await.clone();
    let model = if pending.is_some() {
        pending
    } else {
        shared.observed_model.lock().await.clone()
    };
    Ok(capabilities_from(&initialize, model.as_deref()))
}

/// Start one `claude` process and hand back the session it holds.
///
/// The handshake is awaited here rather than deferred, and that is what makes an argv
/// mistake diagnosable: `claude` exits 1 at parse time for a bad `--permission-mode`, so a
/// session that never initializes is the only evidence, and reporting it as a timeout sent
/// the investigation to the link layer while the cause sat in one argv entry.
pub async fn run(spec: RunSpec) -> Result<Box<dyn Session>> {
    run_with(&[super::AGENT_NAME.to_string()], spec).await
}

/// [`run`], with the program that answers named separately.
///
/// **The seam a test needs, and the reason it is here rather than an environment
/// variable.** `AGENTS.md`: a test needing the real CLI is a test that does not run — it
/// wants a login, spends real tokens, and only sees whatever shapes one live turn happens
/// to produce, which is how every classification bug in this adapter's history stayed
/// invisible to its end-to-end check. So the session half is tested against a scripted
/// stdio peer, and this is where the peer is substituted.
///
/// `launcher[0]` is the program and the rest are prepended arguments, so a peer can be
/// `["sh", "/tmp/peer.sh"]`. Everything after them is the argv
/// [`super::ClaudeAdapter::argv_for`] built, unchanged — which is deliberate: a test peer
/// therefore receives the real command line and a test can assert on it.
pub async fn run_with(launcher: &[String], spec: RunSpec) -> Result<Box<dyn Session>> {
    let native_id = match &spec.native_id {
        RequestedNativeId::Dictate(id) | RequestedNativeId::Resume(id) => Some(id.clone()),
        RequestedNativeId::New => None,
    };

    let (mut argv, warnings) = super::ClaudeAdapter::argv_for(&spec);
    for warning in warnings {
        // Loudly, because the user asked for one policy and gets another, and only this
        // says so. `argv_for` returns them rather than logging so it stays pure.
        warn!("{warning}");
    }

    // The preamble arrives as **text** because the file lives beside the server and the
    // agent runs wherever it does, and this CLI wants a path. `argv_for` cannot write one
    // — it is pure — so it emits a marker and the path is substituted here.
    let preamble = match (spec.inject_identity, spec.identity_preamble.as_deref()) {
        (true, Some(text)) => {
            let path = std::env::temp_dir().join(format!(
                "sylph-claude-soul-{}-{}.md",
                std::process::id(),
                native_id.as_ref().map_or("new", NativeId::as_str)
            ));
            std::fs::write(&path, text)
                .with_context(|| format!("writing the identity preamble to {}", path.display()))?;
            for part in argv.iter_mut() {
                if part == super::IDENTITY_PREAMBLE_PLACEHOLDER {
                    *part = path.to_string_lossy().to_string();
                }
            }
            Some(path)
        }
        _ => None,
    };

    // argv[0] is the agent name; the launcher replaces it, and `Child` takes the program
    // separately from its arguments.
    argv.remove(0);
    let mut launcher = launcher.to_vec();
    let program = if launcher.is_empty() {
        super::AGENT_NAME.to_string()
    } else {
        launcher.remove(0)
    };
    launcher.extend(argv);
    let (child, lines) = Child::spawn(Spawn {
        program,
        args: launcher,
        cwd: spec.workdir.clone(),
        env: HashMap::new(),
        // See [`reply_id`], and this module's header for what each of its three conditions
        // prevents. The short form: the id is nested, the vendor raises requests on this
        // same channel, and the channel carries other clients' replies too.
        reply_id: Some(Box::new(reply_id)),
    })
    .await?;

    let control = Arc::new(Control { child });
    let shared = Arc::new(Shared {
        native_id: std::sync::RwLock::new(native_id.clone()),
        pending_model: Mutex::new(spec.flags.get("model").cloned()),
        ..Shared::default()
    });
    // Bounded. A turn streams thousands of deltas, and a channel small enough to block
    // would make the pump's backpressure part of what a duration measures.
    let (tx, rx) = mpsc::channel(1024);
    tokio::spawn(pump(lines, tx.clone(), control.clone(), shared.clone()));
    tokio::spawn(bundle_pump(tx.clone(), shared.clone()));

    // The handshake that turns a process into a session. This is where the project's real
    // command list comes from, and where the dialog kinds are declared — the declaration is
    // load-bearing: the CLI's schema treats absence as "cannot display" and fails closed,
    // so without it `/plan` returns a paragraph instead of an approve/reject choice.
    //
    // First-attached-client-wins per that same schema, so this call is the only one whose
    // declaration counts and a later re-initialize cannot widen it.
    control
        .send(
            "initialize",
            json!({"supportedDialogKinds": SUPPORTED_DIALOG_KINDS}),
        )
        .await
        .context("claude did not complete the initialize handshake")?;

    Ok(Box::new(ClaudeSession {
        control,
        shared,
        stream: Mutex::new(Some(rx)),
        preamble,
    }))
}

impl ClaudeSession {
    /// Apply one config option, validating what the vendor does not.
    ///
    /// # Two hazards, and the second is the one this vendor adds
    ///
    /// **A vendor's `success` is not a promise it did what you asked.** *Measured on
    /// 2.1.251:* `set_model` with `"banana-9000"` answers **success**, carries no
    /// confirmation, and a later `initialize` reports no model at all — so a bogus value is
    /// indistinguishable from an applied one at every observable layer. `apply_flag_settings`
    /// with `{"effort":"banana"}` likewise answers success. This is the `codex` `effort`
    /// hazard on a second vendor, and the rule `AGENTS.md` draws from it applies verbatim:
    /// anything the vendor does not itself validate must be validated adapter-side against
    /// what it enumerated, or a refused write is indistinguishable from an applied one.
    ///
    /// `set_permission_mode` is the exception that proves the shape — it *does* validate,
    /// answering `error` with the accepted list — so it is checked here only against
    /// [`SETTABLE_PERMISSION_MODES`], which is narrower for a measured reason: `manual`
    /// parses and is silently normalized to `default`.
    async fn apply_config(&self, id: &str, value: Value) -> Result<()> {
        // The catalogue is the enumeration to validate against, and it has to come from the
        // vendor rather than from a constant: it depends on the account.
        let need_catalogue = matches!(id, "model" | "effort");
        let initialize = if need_catalogue {
            self.control.send("initialize", json!({})).await?
        } else {
            Value::Null
        };

        match id {
            "model" => {
                let models = models_from(&initialize);
                let Some(requested) = value.as_str() else {
                    return Err(anyhow::Error::new(AdapterError::invalid_value(
                        "a model is named by a string",
                    )));
                };
                if !models.iter().any(|model| model.value == requested) {
                    // `INVALID_VALUE`, not `UNSUPPORTED_FIELD`: the option exists and this
                    // value does not. An error that names the wrong layer sends the
                    // investigation to the wrong layer — answering "no such option" here
                    // would retire a picker whose other entries all work.
                    return Err(anyhow::Error::new(AdapterError::invalid_value(format!(
                        "claude does not offer the model {requested:?} on this account"
                    ))));
                }
                self.control
                    .send("set_model", json!({"model": requested}))
                    .await?;
                // `set_model` succeeds without a confirmation payload and a
                // subsequent `initialize` still omits the current model.  The
                // value was validated against that very catalogue, so retain it
                // as a pending adapter-local selection until `system/init`
                // reports the vendor's resolved id.
                *self.shared.pending_model.lock().await = Some(requested.to_string());
            }
            "effort" => {
                let Some(requested) = value.as_str() else {
                    return Err(anyhow::Error::new(AdapterError::invalid_value(
                        "an effort level is named by a string",
                    )));
                };
                // Scoped to the **selected** model, because `supportedEffortLevels` is a
                // field on a model entry: a session-global effort enumeration is wrong for
                // any account whose models differ.
                let pending = self.shared.pending_model.lock().await.clone();
                let model = if pending.is_some() {
                    pending
                } else {
                    self.shared.observed_model.lock().await.clone()
                };
                let offered = config_from(&initialize, model.as_deref());
                let Some(option) = offered.iter().find(|option| option.id == "effort") else {
                    return Err(anyhow::Error::new(AdapterError::unsupported_field(
                        "the selected model reports no effort levels",
                    )));
                };
                if !option.accepts(&value) {
                    return Err(anyhow::Error::new(AdapterError::invalid_value(format!(
                        "claude's selected model does not offer the effort level {requested:?}"
                    ))));
                }
                // Measured against the real CLI rather than inferred from the name:
                // `apply_flag_settings` refuses a bare field with "requires settings to be
                // an object", and `set_max_thinking_tokens` wants an integer budget rather
                // than an effort name. Two wrong guesses, both of which would have failed
                // silently as a setting that never took.
                self.control
                    .send(
                        "apply_flag_settings",
                        json!({"settings": {"effort": requested}}),
                    )
                    .await?;
            }
            "fastMode" => {
                let Some(on) = value.as_bool() else {
                    return Err(anyhow::Error::new(AdapterError::invalid_value(
                        "fast mode is a boolean",
                    )));
                };
                self.control
                    .send("apply_flag_settings", json!({"settings": {"fastMode": on}}))
                    .await?;
            }
            "permissionMode" => {
                let Some(requested) = value.as_str() else {
                    return Err(anyhow::Error::new(AdapterError::invalid_value(
                        "a permission mode is named by a string",
                    )));
                };
                if !SETTABLE_PERMISSION_MODES.contains(&requested) {
                    return Err(anyhow::Error::new(AdapterError::invalid_value(format!(
                        "claude does not apply the permission mode {requested:?} as given; \
                         it accepts {}",
                        SETTABLE_PERMISSION_MODES.join(", ")
                    ))));
                }
                let confirmed = self
                    .control
                    .send("set_permission_mode", json!({"mode": requested}))
                    .await?;
                // *Measured on 2.1.251:* the success payload is `{"mode": "<mode>"}`, and
                // the CLI is entitled to answer with a mode other than the one asked for —
                // `manual` answers success and confirms `default`. Nothing is written from
                // the request here; the surface is re-read from the vendor by
                // `set_config`, so the confirmation only has to be *reported* when it
                // disagrees.
                if let Some(applied) = confirmed.get("mode").and_then(Value::as_str) {
                    if applied != requested {
                        warn!(
                            requested,
                            applied, "claude applied a different permission mode"
                        );
                    }
                }
            }
            // `UNSUPPORTED_FIELD` is the right code here and the distinction is
            // load-bearing: it means "no such option", which justifies disabling the
            // control, where `INVALID_VALUE` must leave it usable.
            _ => {
                return Err(anyhow::Error::new(AdapterError::unsupported_field(
                    format!("'{id}' is not settable on a claude session"),
                )))
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Session for ClaudeSession {
    fn native_id(&self) -> Option<NativeId> {
        self.shared.native_id.read().ok().and_then(|id| id.clone())
    }

    async fn capabilities(&self) -> Result<SessionCapabilities> {
        refreshed_surface(&self.control, &self.shared).await
    }

    /// Accept a turn.
    ///
    /// Returns when the agent has **accepted** it, not when it has finished: this CLI's
    /// stdin stays open for the session's life, so a write is the acceptance and the
    /// outcome arrives on [`Session::events`] as a `result`.
    ///
    /// # What a client observes if the CLI never answers, and why nothing is invented here
    ///
    /// A write is the only acknowledgement this vendor gives, so a wedged CLI presents as a
    /// turn that opened and never spoke: the session stays `running`, the composer keeps
    /// showing Stop, and no event explains it. That is the silence `AGENTS.md` says nothing
    /// may produce by default, and it is **not fixed here** — synthesizing a timeout would
    /// report an outcome the vendor never stated, which is the defect that put a hardcoded
    /// `stopReason` in `pi`'s adapter. The honest state is a gap in the vocabulary, and it
    /// belongs in the roadmap rather than in a fabricated event.
    ///
    /// One real cause of it *is* reportable rather than unknowable, which narrows the claim:
    /// the CLI collects `streamCloseInterruptRequestIds` and fires
    /// `onStreamClosedWithParkedQuestion` when its **input stream closes while a
    /// `can_use_tool` question is parked**, rejecting each pending request with `Tool
    /// permission stream closed before response received`. Note what that does and does not
    /// cover: it is *this side* closing stdin, which for a Sylph session happens only at
    /// [`Session::close`]. So it explains a silence the adapter itself would have caused, not
    /// one it can observe in a CLI that wedged on its own. Useful, and less than it sounds.
    async fn send(&self, turn: TurnInput) -> Result<()> {
        // Config first, so the turn runs under the settings it was sent with. Applied here
        // rather than as preceding calls from the server: one message means there is no
        // window in which a turn could begin under a mix of old and new values.
        for (id, value) in &turn.config {
            if let Err(error) = self.apply_config(id, value.clone()).await {
                // **Not fatal.** An agent refusing a setting it no longer offers is worth
                // reporting, but losing the turn over it is worse than running under the
                // previous value.
                warn!(%error, id, "could not apply a per-turn config override");
            }
        }

        let text = match &turn.command {
            // A **prompt** command is a kind of turn, not a side channel, so its output
            // lands in the transcript like anything else. A control command never reaches
            // here: it resolves against typed config.
            Some(command) => {
                let args = turn
                    .args
                    .values()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("/{command} {args}").trim().to_string()
            }
            None => turn.text.clone(),
        };

        self.control
            .child
            .send(json!({
                "type": "user",
                "message": {"role": "user", "content": build_content(&text, &turn.attachments)},
            }))
            .await
    }

    /// Send the first turn of a prepared link and wait until Claude reports its id.
    ///
    /// `initialize` exposes capabilities before a user message, but Claude does not put a
    /// session id on that response.  The id arrives on the first `system/init` frame instead;
    /// waiting here keeps that vendor timing inside the adapter rather than making the server
    /// guess or require a fabricated id.  The event pump continues to run while we wait, so
    /// callers must buffer the stream until promotion has created the durable row.
    async fn send_initial(&self, turn: TurnInput) -> Result<NativeId> {
        self.send(turn).await?;
        let deadline = tokio::time::Instant::now() + crate::process::REPLY_TIMEOUT;
        loop {
            if let Some(native) = self.native_id() {
                return Ok(native);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "claude accepted the first turn but did not report a session id within {}s",
                    crate::process::REPLY_TIMEOUT.as_secs()
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    async fn interrupt(&self) -> Result<()> {
        self.control.send("interrupt", json!({})).await?;
        Ok(())
    }

    /// Refused: **this vendor has no steer.**
    ///
    /// *Measured on 2.1.251:* the control protocol's subtypes are `interrupt`, `initialize`,
    /// `can_use_tool`, `set_permission_mode`, `set_model`, and `mcp_message`, each occurring
    /// 12–56 times as a quoted token in the binary, while `"steer"` occurs **zero** times.
    ///
    /// The only mid-turn channel is a second `user` message on the held-open stdin, and that
    /// is **queueing rather than steering** — decidable from the shape without a live probe:
    /// `--input-format stream-json` reads stdin through a single async iterator that the turn
    /// loop awaits, so a write during a turn is not read until the turn ends. See
    /// [`Session::queue`], which is where that write now lives.
    ///
    /// Declaring [`sylph_protocol::Capability::Steer`] here was a shipped defect rather than a
    /// mislabelling.
    /// The composer sends a steer whenever a turn is live and the agent claims the
    /// capability, so a user's correction arrived *after* the thing it was meant to prevent,
    /// with every layer reporting success. Refusing is what makes the composer fall back to
    /// the behaviour the vendor can actually deliver.
    async fn steer(&self, _text: &str, _expected_turn_id: Option<&str>) -> Result<()> {
        Err(anyhow::Error::new(AdapterError::unsupported_field(
            "claude has no steer: text sent mid-turn is not read until the turn ends, so it \
             queues rather than corrects. Send it as a queued follow-up instead."
                .to_string(),
        )))
    }

    /// Queue text for after the running turn: a second `user` message on stdin.
    ///
    /// This is the write that used to be called steering, under the name that describes when
    /// the agent reads it. Nothing about the mechanism changed — one async iterator, read
    /// again once the turn loop comes back for it.
    ///
    /// **The vendor reports nothing about the queue**, so a client cannot show what is
    /// waiting on this agent the way `pi`'s `queue_update` allows. That is a real difference
    /// between two agents offering the same capability, and it belongs at the code rather
    /// than in a client's assumption.
    async fn queue(&self, text: &str) -> Result<()> {
        self.control
            .child
            .send(json!({"type": "user", "message": {"role": "user", "content": text}}))
            .await
    }

    /// Set one option and answer with the **refreshed array**.
    ///
    /// Not an acknowledgement, because setting one option can change another's choices:
    /// this CLI carries `supportedEffortLevels` per model, so a model change can shrink or
    /// grow the effort option.
    ///
    /// The refresh is a *re-read from the vendor*, which is what makes the confirmed value
    /// the vendor's and not the request's.
    async fn set_config(&self, id: &str, value: Value) -> Result<Vec<ConfigOption>> {
        if let Err(error) = self.apply_config(id, value).await {
            // A refusal the vendor stated is `INVALID_VALUE`: the option exists and this
            // value was rejected, so the control must stay usable. Recovered by
            // `downcast_ref` rather than by matching the message, because the text is the
            // vendor's and may be reworded while the code is declared protocol.
            if let Some(refusal) = error.downcast_ref::<VendorRefused>() {
                return Err(anyhow::Error::new(AdapterError::invalid_value(format!(
                    "claude refused to set '{id}': {refusal}"
                ))));
            }
            return Err(error);
        }
        Ok(refreshed_surface(&self.control, &self.shared).await?.config)
    }

    fn launch_flag_for_config(&self, id: &str, value: &Value) -> Option<(String, String)> {
        let value = value.as_str()?.to_string();
        match id {
            // These controls have the same launch vocabulary and are safe to carry
            // across a detach/reattach. `permissionMode` deliberately maps to the
            // adapter's launch-only spelling; the server must not know that the two
            // names describe one Claude setting.
            "model" | "effort" => Some((id.to_string(), value)),
            "permissionMode" if super::LAUNCH_PERMISSION_MODES.contains(&value.as_str()) => {
                Some(("mode".to_string(), value))
            }
            // Output style and fast mode are process controls with no launch flag.
            _ => None,
        }
    }

    /// Answer a request this session raised.
    async fn resolve(&self, resolution: Resolution) -> Result<()> {
        // Taken rather than read: a request is answered once, and leaving the entry would
        // let a second resolution send a second `control_response` for an id the CLI has
        // already resolved — which its own logs call out ("control_response for
        // already-resolved toolUseID").
        let kind = self.shared.asks.lock().await.remove(&resolution.request_id);
        let Some(kind) = kind else {
            // **Not guessed.** The response shape differs per kind and sending the wrong
            // one parks the request until its deadline, which reads as the agent hanging.
            // A resolution for a request this session never raised is a bug, and refusing
            // says so where a default would hide it.
            return Err(anyhow!(
                "claude has no pending request {}",
                resolution.request_id
            ));
        };
        let body = answer_payload(&kind, &resolution);
        self.control
            .reply(&resolution.request_id, "success", body)
            .await
    }

    fn events(&mut self) -> Option<EventStream> {
        // `try_lock` because this is the sync method on an otherwise async trait, and the
        // lock is uncontended by construction: `&mut self` means nothing else holds this
        // session. Taking the receiver is what makes a second call answer `None`.
        let receiver = self.stream.try_lock().ok()?.take()?;
        Some(Box::pin(futures::stream::unfold(
            receiver,
            |mut receiver| async move { receiver.recv().await.map(|item| (item, receiver)) },
        )))
    }

    /// Ask the agent to exit, and make sure it did.
    ///
    /// Idempotent and **escalates unconditionally** — [`Child::close`] signals the whole
    /// process group with SIGTERM and then SIGKILL, because a `close` a vendor ignores
    /// would leak a process for the server's life, and signalling an already-exited group
    /// is defined to be harmless.
    async fn close(&self) -> Result<()> {
        self.shared
            .closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.control.child.close().await;
        if let Some(path) = &self.preamble {
            // Best effort: a leftover file in the temp directory is not worth failing a
            // close over, and the close is what the caller is waiting on.
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }
}

/// One user message's content: images inline, other files named by path.
///
/// **Images have to be base64 content blocks** — the `Read` tool is unreliable on binary
/// image data — while text and PDFs are better left to the CLI's own tools, which handle
/// them well and do not bloat the message. So the split is by what the vendor reads well,
/// not by size.
///
/// Returns a bare string when there are no images, because that is the shape the CLI
/// accepts for the ordinary case and a one-element block array would be a gratuitous
/// difference in every transcript.
fn build_content(text: &str, attachments: &[String]) -> Value {
    let mut images: Vec<(&String, &'static str)> = Vec::new();
    let mut others: Vec<&String> = Vec::new();
    for path in attachments {
        match crate::native::image_mime(std::path::Path::new(path)) {
            Some(mime) => images.push((path, mime)),
            None => others.push(path),
        }
    }

    let body = prefix_paths(&others, text);
    if images.is_empty() {
        return Value::String(body);
    }

    let mut blocks = Vec::new();
    for (path, mime) in images {
        match std::fs::read(path) {
            Ok(bytes) => blocks.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime,
                    "data": BASE64.encode(&bytes),
                },
            })),
            // Skipped with a reason rather than failing the turn: an unreadable attachment
            // is worth saying and is not worth losing the message over.
            Err(error) => warn!(%error, path, "skipping an unreadable image attachment"),
        }
    }
    if !body.is_empty() {
        blocks.push(json!({"type": "text", "text": body}));
    }
    Value::Array(blocks)
}

/// Name the non-image attachments above the message, so the agent can read them itself.
fn prefix_paths(paths: &[&String], text: &str) -> String {
    if paths.is_empty() {
        return text.to_string();
    }
    let mut lines =
        vec!["Files attached for this turn (absolute paths on local disk):".to_string()];
    lines.extend(paths.iter().map(|path| format!("- {path}")));
    lines.push(String::new());
    lines.push(text.to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;
    use sylph_protocol::{EventKind, Phase, RefusalCode, RequestKind};

    /// A scripted `claude` peer: real stdio, real framing, no vendor and no tokens.
    ///
    /// **`AGENTS.md`: a test needing the real CLI is a test that does not run.** It wants a
    /// login, spends tokens, and only produces whatever shapes one live turn happens to
    /// take — which is precisely how every classification bug in this adapter's history
    /// stayed invisible to a live run. So the peer answers the control protocol as the real
    /// CLI was measured answering it, **including where it lies**:
    ///
    /// - `manual` answers **success** and confirms `default`.
    /// - `bypassPermissions` answers `subtype: "error"` with **no `response` member**.
    /// - `set_model` and `apply_flag_settings` answer success for anything, with no
    ///   confirmation — which is what makes adapter-side validation observable.
    ///
    /// Every line the adapter writes is appended to `log`, so a test asserts on the **exact
    /// wire bytes** rather than on whether a call returned. That is the `selectedOptions`
    /// lesson: "it returned Ok" and "the vendor received the answer" are different claims.
    const PEER: &str = r#"
log="$1"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"subtype":"initialize"'*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"commands":[{"name":"model","description":"Set the model","argumentHint":""},{"name":"verify","description":"Check it","argumentHint":"[target]"}],"agents":[{"name":"Explore","description":"Search"}],"models":[{"value":"default","displayName":"Default (recommended)","supportedEffortLevels":["low","high"]},{"value":"sonnet","displayName":"Sonnet","supportedEffortLevels":["low","medium","high"]}],"current_permission_mode":"%s","fast_mode_state":"off","output_style":"default"}}}\n' "$id" "${MODE:-default}"
      ;;
    *'"subtype":"set_permission_mode"'*)
      asked=$(printf '%s' "$line" | sed -n 's/.*"mode":"\([^"]*\)".*/\1/p')
      case "$asked" in
        bypassPermissions)
          printf '{"type":"control_response","response":{"subtype":"error","request_id":"%s","error":"Cannot set permission mode to bypassPermissions because the session was not launched with --dangerously-skip-permissions"}}\n' "$id"
          ;;
        manual)
          printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"mode":"default"}}}\n' "$id"
          ;;
        *)
          MODE="$asked"
          printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"mode":"%s"}}}\n' "$id" "$MODE"
          ;;
      esac
      ;;
    *'"subtype":"interrupt"'*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"still_queued":[]}}}\n' "$id"
      ;;
    *'"subtype":"set_model"'*|*'"subtype":"apply_flag_settings"'*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s"}}\n' "$id"
      ;;
    *'"type":"user"'*)
      # Nested, because a two-part `sh` pattern is **ordered** and this JSON's keys are
      # not in the order they were written: `serde_json` sorts them, so `content` precedes
      # `type` and a pattern reading `*"type":"user"*ASKME*` never matches — it fell
      # through to the plain-turn branch instead. The same shape assumption as the
      # vendor-key family, appearing in the fixture: it compiled, and it silently answered
      # the wrong thing.
      case "$line" in
        *ASKME*)
          printf '{"type":"control_request","request_id":"vendor-1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"toolu_1","input":{"command":"ls"}}}\n'
          ;;
        *QUESTION*)
          printf '{"type":"control_request","request_id":"vendor-q","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"toolu_q","input":{"questions":[{"question":"Indentation?","options":[{"label":"Tabs"},{"label":"Spaces"}]}]}}}\n'
          ;;
        *FUTURE*)
          printf '{"type":"control_request","request_id":"vendor-f","request":{"subtype":"some_future_ask"}}\n'
          ;;
        *CHANGED*)
          printf '{"type":"system","subtype":"commands_changed"}\n'
          ;;
        *)
          printf '{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1","role":"assistant"}}}\n'
          printf '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text"}}}\n'
          printf '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}}\n'
          printf '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}\n'
          printf '{"type":"result","subtype":"success","result":"done","duration_ms":12,"usage":{"input_tokens":3}}\n'
          ;;
      esac
      ;;
  esac
done
"#;

    /// A peer that reports a resolved model only when the first turn starts.
    ///
    /// This is the ordering the real CLI uses: `set_model` has an empty success
    /// response, while `system/init` carries the resolved model (including the
    /// optional `[1m]` context suffix). The adapter's pending selection must be
    /// replaced by that report and effort levels rebuilt from its catalogue entry.
    const MODEL_PEER: &str = r#"
log="$1"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"subtype":"initialize"'*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"commands":[{"name":"model","description":"Set the model","argumentHint":""}],"models":[{"value":"default","displayName":"Default","resolvedModel":"claude-opus-5[1m]","supportedEffortLevels":["low","high"]},{"value":"sonnet[1m]","displayName":"Sonnet 5 (1M)","resolvedModel":"claude-sonnet-5[1m]","supportedEffortLevels":["low","medium","high"]},{"value":"haiku","displayName":"Haiku"}],"current_permission_mode":"default","fast_mode_state":"off"}}}\n' "$id"
      ;;
    *'"subtype":"set_model"'*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s"}}\n' "$id"
      ;;
    *'"type":"user"'*)
      printf '{"type":"system","subtype":"init","session_id":"claude-model-observed","model":"claude-sonnet-5[1m]","permissionMode":"default"}\n'
      printf '{"type":"result","subtype":"success","result":"done"}\n'
      ;;
  esac
done
"#;

    /// A peer that dies immediately, for the paths that must diagnose a dead vendor.
    const DEAD_PEER: &str = "echo 'claude: bad argument' >&2; exit 1";

    struct Peer {
        session: Box<dyn Session>,
        log: PathBuf,
        script: PathBuf,
    }

    /// Enough for `expect_err`, which needs the `Ok` side to be printable. A session has no
    /// meaningful debug form and the paths are what a failure wants to name.
    impl std::fmt::Debug for Peer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Peer")
                .field("log", &self.log)
                .field("script", &self.script)
                .finish()
        }
    }

    impl Peer {
        /// Every line the adapter wrote to the vendor's stdin, parsed.
        fn written(&self) -> Vec<Value> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("the adapter writes JSON"))
                .collect()
        }

        /// The `request` bodies of every control the adapter sent with this subtype.
        ///
        /// **Does not wait**, which is what makes it usable for an *absence* assertion —
        /// and such an assertion is sound here for a stated reason rather than by luck: the
        /// peer logs each line before replying to it, and it reads stdin in order. So a
        /// reply the adapter has already received proves the peer has logged everything sent
        /// before that request. Every negative assertion below is placed after such a reply.
        fn controls(&self, subtype: &str) -> Vec<Value> {
            self.written()
                .into_iter()
                .filter(|line| line["type"] == "control_request")
                .filter(|line| line["request"]["subtype"] == subtype)
                .map(|line| line["request"].clone())
                .collect()
        }

        /// The `response` envelopes the adapter sent back to the vendor.
        fn replies(&self) -> Vec<Value> {
            self.written()
                .into_iter()
                .filter(|line| line["type"] == "control_response")
                .map(|line| line["response"].clone())
                .collect()
        }

        /// Wait until the adapter has written a line matching `want`, then return it.
        ///
        /// Needed wherever the assertion is about a write with **no reply to wait on** — a
        /// turn and a steer are both fire-and-forget, so reading the log immediately races
        /// the peer's own append. Bounded, so a genuinely missing write fails the test
        /// rather than hanging the suite.
        async fn awaited(&self, want: impl Fn(&Value) -> bool) -> Value {
            for _ in 0..250 {
                if let Some(found) = self.written().into_iter().find(&want) {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!(
                "the adapter never wrote the expected line: {:?}",
                self.written()
            );
        }

        /// Wait for the user message the adapter wrote.
        async fn user_message(&self) -> Value {
            self.awaited(|line| line["type"] == "user").await
        }

        /// Wait for the reply the adapter sent to one vendor request.
        async fn awaited_reply(&self, vendor_id: &str) -> Value {
            self.awaited(|line| {
                line["type"] == "control_response" && line["response"]["request_id"] == vendor_id
            })
            .await["response"]
                .clone()
        }
    }

    impl Drop for Peer {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.log);
            let _ = std::fs::remove_file(&self.script);
        }
    }

    fn spec(tag: &str) -> RunSpec {
        RunSpec {
            workdir: std::env::temp_dir().to_string_lossy().to_string(),
            native_id: RequestedNativeId::Dictate(NativeId::new(format!("native-{tag}"))),
            inject_identity: false,
            identity_preamble: None,
            flags: BTreeMap::new(),
            permission_mode: None,
        }
    }

    async fn peer_with(tag: &str, script: &str, spec: RunSpec) -> Result<Peer> {
        let unique = format!("{}-{}", std::process::id(), tag);
        let log = std::env::temp_dir().join(format!("sylph-claude-peer-{unique}.log"));
        let path = std::env::temp_dir().join(format!("sylph-claude-peer-{unique}.sh"));
        let _ = std::fs::remove_file(&log);
        std::fs::write(&path, script).unwrap();
        // The log path is an **argument, never an environment variable.** `set_var` is
        // process-global and `cargo test` runs these concurrently, so an env var would have
        // every peer writing to whichever path was set last — which failed as thirteen
        // unrelated assertions rather than as a fixture problem. Arguments are per-process,
        // and the launcher's own arguments precede the real argv, which the peer ignores.
        let session = run_with(
            &[
                "sh".to_string(),
                path.to_string_lossy().to_string(),
                log.to_string_lossy().to_string(),
            ],
            spec,
        )
        .await?;
        Ok(Peer {
            session,
            log,
            script: path,
        })
    }

    async fn peer(tag: &str) -> Peer {
        peer_with(tag, PEER, spec(tag))
            .await
            .expect("the peer initializes")
    }

    /// The predicate's three conditions, each pinned on the frame it has to reject.
    ///
    /// A unit test rather than only an end-to-end one because direction is a **vendor fact**,
    /// and the closure form of `reply_id` exists precisely so that fact is code rather than a
    /// comment. Code deserves a test.
    #[test]
    fn the_reply_predicate_answers_only_for_our_own_replies() {
        // A reply to us: the id is inside `response`, prefixed, and comes back as the
        // decimal `Child::request` correlates on.
        assert_eq!(
            reply_id(&json!({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": "sylph-7", "response": {}},
            })),
            Some("7".to_string())
        );

        // A refusal is still a reply and must still correlate — the discriminant is an
        // outcome for `Control::send` to read, never a routing question.
        assert_eq!(
            reply_id(&json!({
                "type": "control_response",
                "response": {"subtype": "error", "request_id": "sylph-7", "error": "no"},
            })),
            Some("7".to_string())
        );

        // **A request the vendor raised.** It carries `request_id` too, at the top level.
        // Answering here routes an approval into a waiter: nobody is asked, the turn waits
        // on a human forever, and the caller receives an approval as its reply.
        assert_eq!(
            reply_id(&json!({
                "type": "control_request",
                "request_id": "sylph-7",
                "request": {"subtype": "can_use_tool"},
            })),
            None
        );

        // Another client's reply, forwarded by the CLI — its own logs say it does this. The
        // id is from an id space this session does not own, so it must not consume a waiter.
        assert_eq!(
            reply_id(&json!({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": "device-42"},
            })),
            None
        );

        // An event, which is most of the stream.
        assert_eq!(
            reply_id(&json!({"type": "result", "subtype": "success"})),
            None
        );
        // A top-level id, which is where it is *not*: answering would mean the nesting was
        // never read.
        assert_eq!(
            reply_id(&json!({"type": "control_response", "request_id": "sylph-7"})),
            None
        );
    }

    /// **A vendor request whose id collides with an outstanding one of ours still reaches the
    /// stream.**
    ///
    /// The end-to-end form of the predicate's first condition, and it is worth the fixture
    /// because both halves of the failure are silent. The peer answers `set_permission_mode`
    /// by first raising an approval **under the same id** and only then replying. A predicate
    /// reading the id alone hands that approval to the waiting `set_config` — which then
    /// reports a confirmation it invented from an approval — and the real reply arrives to an
    /// empty map and is dropped, while the approval never reaches any client, so nobody is
    /// asked and the turn waits on a human that was never told.
    #[tokio::test]
    async fn a_vendor_request_reusing_our_id_is_not_swallowed_as_a_reply() {
        const COLLIDES: &str = r#"
log="$1"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"subtype":"initialize"'*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"current_permission_mode":"%s"}}}\n' "$id" "${MODE:-default}"
      ;;
    *'"subtype":"set_permission_mode"'*)
      MODE=plan
      # The approval first, under **our** id, and the reply after it.
      printf '{"type":"control_request","request_id":"%s","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"toolu_c","input":{"command":"ls"}}}\n' "$id"
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"mode":"plan"}}}\n' "$id"
      ;;
  esac
done
"#;
        let mut peer = peer_with("collides", COLLIDES, spec("collides"))
            .await
            .unwrap();
        let stream = peer.session.events().unwrap();

        // The reply must be the vendor's confirmation, not the approval.
        let refreshed = peer
            .session
            .set_config("permissionMode", json!("plan"))
            .await
            .expect("the confirmation is what correlates");
        let mode = refreshed
            .iter()
            .find(|option| option.id == "permissionMode")
            .unwrap();
        assert_eq!(mode.current_value, Some(json!("plan")));

        // And the approval must have reached the stream, so somebody is asked.
        let request = first_request(stream)
            .await
            .expect("the approval reaches a client");
        assert_eq!(request.kind, RequestKind::Approval);
        assert_eq!(request.tool.as_deref(), Some("Bash"));
    }

    /// An attachment's split, and the encoding under it.
    ///
    /// **Images are inlined as base64 and everything else is named by path**, because that is
    /// what each kind reaches the model best as: the `Read` tool is unreliable on binary image
    /// data, while text and PDFs are handled well by the CLI's own tools and inlining them
    /// only bloats the message.
    ///
    /// The expected base64 is written out rather than computed with the same encoder the code
    /// uses — a test that encodes with `BASE64` to check `BASE64` proves only that it agrees
    /// with itself. These are `base64.standard_b64encode` from Python over the same bytes.
    #[test]
    fn an_image_attachment_is_inlined_and_other_files_are_named() {
        let dir = std::env::temp_dir().join(format!("sylph-claude-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("shot.png");
        // A real PNG signature, because `image_mime` sniffs the header rather than trusting
        // the extension — a wrong guess there sends a binary blob as text.
        std::fs::write(
            &png,
            [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, b'A', b'B'],
        )
        .unwrap();
        let notes = dir.join("notes.txt");
        std::fs::write(&notes, "not an image").unwrap();

        let content = build_content(
            "look at these",
            &[
                png.to_string_lossy().to_string(),
                notes.to_string_lossy().to_string(),
            ],
        );
        let blocks = content
            .as_array()
            .expect("images make it an array of blocks");
        assert_eq!(blocks[0]["type"], json!("image"));
        assert_eq!(blocks[0]["source"]["media_type"], json!("image/png"));
        assert_eq!(blocks[0]["source"]["data"], json!("iVBORw0KGgpBQg=="));
        // The text block comes last and names the non-image file, so the agent can read it
        // with its own tools.
        let text = blocks[1]["text"].as_str().unwrap();
        assert!(text.contains("notes.txt"), "got: {text}");
        assert!(text.ends_with("look at these"), "got: {text}");

        // A `.png` name over non-image bytes is **not** an image: the header decides.
        let liar = dir.join("liar.png");
        std::fs::write(&liar, "plain text pretending").unwrap();
        let content = build_content("hi", &[liar.to_string_lossy().to_string()]);
        assert!(
            content.is_string(),
            "a misnamed file must not be inlined as an image: {content}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no attachments the content is a bare string, not a one-element block array —
    /// which is the shape the CLI accepts for the ordinary case, and avoids a gratuitous
    /// difference in every transcript.
    #[test]
    fn a_plain_turn_is_a_bare_string() {
        assert_eq!(build_content("hello", &[]), json!("hello"));
    }

    /// Identity is known **before the first event**, from the dictated id.
    #[tokio::test]
    async fn the_native_id_is_the_dictated_one() {
        let peer = peer("native-id").await;
        assert_eq!(
            peer.session.native_id().as_ref().unwrap().as_str(),
            "native-native-id"
        );
    }

    /// A session launched without a dictated id remains nameless until Claude reports one.
    #[tokio::test]
    async fn an_unnamed_session_waits_for_a_vendor_identity() {
        let mut unnamed = spec("unnamed");
        unnamed.native_id = RequestedNativeId::New;
        let peer = peer_with("unnamed", PEER, unnamed)
            .await
            .expect("initializes");
        assert!(peer.session.native_id().is_none());
    }

    /// **The handshake is awaited, so an argv mistake is diagnosable.**
    ///
    /// `claude` exits 1 at parse time for a bad flag, and the adapter that reported this as
    /// "did not answer initialize in time" sent the investigation to the link layer while
    /// the cause sat in one argv entry. The stderr tail is what turns it into a diagnosis.
    #[tokio::test]
    async fn a_vendor_that_dies_at_launch_fails_run_naming_what_it_said() {
        let error = peer_with("dead", DEAD_PEER, spec("dead"))
            .await
            .expect_err("a dead vendor cannot initialize");
        let text = format!("{error:#}");
        assert!(text.contains("initialize"), "got: {text}");
        assert!(
            text.contains("bad argument"),
            "the vendor's own stderr is the diagnosis, got: {text}"
        );
    }

    /// The dialog-kind declaration is load-bearing: the CLI's schema treats absence as
    /// "cannot display" and fails closed, so `/plan` returns a paragraph instead of a choice.
    #[tokio::test]
    async fn the_handshake_declares_the_dialog_kinds_this_adapter_can_render() {
        let peer = peer("declare").await;
        let sent = peer.controls("initialize");
        assert_eq!(sent.len(), 1, "one handshake, got {sent:?}");
        let declared = sent[0]["supportedDialogKinds"]
            .as_array()
            .expect("the kinds are declared");
        assert!(declared
            .iter()
            .any(|kind| kind == "permission_ask_user_question"));
        assert_eq!(declared.len(), SUPPORTED_DIALOG_KINDS.len());
    }

    /// Read from the **vendor**, not from what was last set.
    #[tokio::test]
    async fn capabilities_are_read_from_the_vendor() {
        let peer = peer("caps").await;
        let surface = peer.session.capabilities().await.unwrap();
        assert_eq!(surface.permission_mode.as_deref(), Some("default"));
        assert_eq!(surface.commands.len(), 2);
        assert_eq!(surface.subagents.len(), 1);
        let model = surface.config_option("model").expect("a model option");
        assert_eq!(
            model
                .options
                .iter()
                .map(|c| c.value.as_str())
                .collect::<Vec<_>>(),
            vec!["default", "sonnet"]
        );
        // A second `initialize` went out, which is what "read from the vendor" costs and
        // what makes a change by any other route visible.
        assert_eq!(peer.controls("initialize").len(), 2);
    }

    /// **A refusal is `subtype: "error"` with no `response` member.**
    ///
    /// Reading `response.response or {}` mapped it onto the same empty value a successful
    /// no-payload control returns: `bypassPermissions` was refused, recorded as applied,
    /// answered 200, and rendered as the current mode while every tool call went on
    /// prompting. The code must be `INVALID_VALUE` — the option exists and this value was
    /// refused, so the control stays usable — and the vendor's own sentence must survive,
    /// because it names the precondition.
    #[tokio::test]
    async fn a_refused_mode_is_an_invalid_value_carrying_the_vendors_reason() {
        let peer = peer("refused").await;
        let error = peer
            .session
            .set_config("permissionMode", json!("bypassPermissions"))
            .await
            .expect_err("the peer refuses this mode");
        let refusal = error
            .downcast_ref::<AdapterError>()
            .expect("a refusal travels as a typed AdapterError");
        assert_eq!(refusal.code, RefusalCode::InvalidValue);
        assert!(
            refusal.message.contains("--dangerously-skip-permissions"),
            "the vendor's precondition must survive, got: {}",
            refusal.message
        );
    }

    /// A refused set must **not** report the value as applied.
    ///
    /// The observable half of the bug above: the surface after a refusal has to still say
    /// what the session is actually running under.
    #[tokio::test]
    async fn a_refused_mode_leaves_the_surface_saying_what_is_really_running() {
        let peer = peer("refused-surface").await;
        let _ = peer
            .session
            .set_config("permissionMode", json!("bypassPermissions"))
            .await;
        let surface = peer.session.capabilities().await.unwrap();
        assert_eq!(
            surface
                .config_option("permissionMode")
                .unwrap()
                .current_value,
            Some(json!("default")),
            "a refused mode must not be reported as current"
        );
    }

    /// An applied mode **is** reported, read back from the vendor rather than from the
    /// request — which is the same read that makes the refusal above visible.
    #[tokio::test]
    async fn an_applied_mode_is_reported_from_the_vendors_confirmation() {
        let peer = peer("applied").await;
        let refreshed = peer
            .session
            .set_config("permissionMode", json!("plan"))
            .await
            .unwrap();
        let mode = refreshed.iter().find(|o| o.id == "permissionMode").unwrap();
        assert_eq!(mode.current_value, Some(json!("plan")));
    }

    /// **`manual` answers success and confirms `default`**, so it must not be offerable.
    ///
    /// Only a read-back can see this: there is no error. Asserted at the boundary the client
    /// reaches, so a value the vendor silently normalizes cannot be selected at all.
    #[tokio::test]
    async fn a_mode_the_vendor_normalizes_is_refused_before_it_is_sent() {
        let peer = peer("manual").await;
        let error = peer
            .session
            .set_config("permissionMode", json!("manual"))
            .await
            .expect_err("manual is not applied as given");
        assert_eq!(
            error.downcast_ref::<AdapterError>().unwrap().code,
            RefusalCode::InvalidValue
        );
        assert!(
            peer.controls("set_permission_mode").is_empty(),
            "a value the vendor normalizes must not reach it"
        );
    }

    /// **The vendor accepts a nonexistent model with `success` and no confirmation.**
    ///
    /// *Measured on 2.1.251:* `set_model` with `"banana-9000"` answers success, and a later
    /// `initialize` reports no model at all — so a bogus write is indistinguishable from an
    /// applied one at every observable layer. This is the `codex` `effort` hazard on a second
    /// vendor, and the only defence is adapter-side validation against what the vendor
    /// enumerated.
    #[tokio::test]
    async fn a_model_outside_the_catalogue_is_refused_before_it_is_sent() {
        let peer = peer("bad-model").await;
        let error = peer
            .session
            .set_config("model", json!("banana-9000"))
            .await
            .expect_err("the catalogue does not offer it");
        assert_eq!(
            error.downcast_ref::<AdapterError>().unwrap().code,
            RefusalCode::InvalidValue
        );
        assert!(
            peer.controls("set_model").is_empty(),
            "an unenumerated model must not reach the vendor, which would accept it"
        );

        // And an enumerated one does reach it, so the validation is a filter rather than a
        // wall.
        peer.session
            .set_config("model", json!("sonnet"))
            .await
            .unwrap();
        assert_eq!(peer.controls("set_model")[0]["model"], json!("sonnet"));
    }

    /// Effort is validated against the **selected model's** levels, not a session-global set.
    #[tokio::test]
    async fn an_effort_outside_the_selected_models_levels_is_refused() {
        let peer = peer("bad-effort").await;
        let error = peer
            .session
            .set_config("effort", json!("banana"))
            .await
            .expect_err("the model does not offer it");
        assert_eq!(
            error.downcast_ref::<AdapterError>().unwrap().code,
            RefusalCode::InvalidValue
        );
        assert!(peer.controls("apply_flag_settings").is_empty());

        // The vendor wants `{"settings": {...}}`, measured — a bare field is refused with
        // "requires settings to be an object", and `set_max_thinking_tokens` wants an integer
        // budget rather than an effort name. Two wrong guesses, both silent.
        peer.session
            .set_config("effort", json!("high"))
            .await
            .unwrap();
        assert_eq!(
            peer.controls("apply_flag_settings")[0]["settings"]["effort"],
            json!("high")
        );
    }

    /// **The two refusal codes are not interchangeable.**
    ///
    /// `UNSUPPORTED_FIELD` means "no such option" and justifies disabling the control;
    /// `INVALID_VALUE` means "this value, right now" and must leave it usable.
    #[tokio::test]
    async fn an_unknown_option_is_unsupported_rather_than_invalid() {
        let peer = peer("unknown-option").await;
        let error = peer
            .session
            .set_config("outputStyle", json!("Concise"))
            .await
            .expect_err("this adapter does not offer it");
        let refusal = error.downcast_ref::<AdapterError>().unwrap();
        assert_eq!(refusal.code, RefusalCode::UnsupportedField);
        assert_ne!(refusal.code, RefusalCode::InvalidValue);
    }

    /// A wrong JSON type is an invalid value, not a missing option.
    #[tokio::test]
    async fn a_boolean_option_refuses_a_string() {
        let peer = peer("bool").await;
        let error = peer
            .session
            .set_config("fastMode", json!("on"))
            .await
            .expect_err("fast mode is a boolean");
        assert_eq!(
            error.downcast_ref::<AdapterError>().unwrap().code,
            RefusalCode::InvalidValue
        );
        peer.session
            .set_config("fastMode", json!(true))
            .await
            .unwrap();
        assert_eq!(
            peer.controls("apply_flag_settings")[0]["settings"]["fastMode"],
            json!(true)
        );
    }

    /// `set_config` answers with the **refreshed array**, not an acknowledgement: setting one
    /// option can change another's choices, since effort levels are per model here.
    #[tokio::test]
    async fn a_set_answers_with_the_refreshed_array() {
        let peer = peer("refresh").await;
        let refreshed = peer
            .session
            .set_config("model", json!("sonnet"))
            .await
            .unwrap();
        assert!(refreshed.iter().any(|option| option.id == "model"));
        assert!(refreshed.iter().any(|option| option.id == "permissionMode"));
    }

    /// **A successful model set is reflected immediately, then superseded by the vendor.**
    ///
    /// `set_model` answers **success** without a payload and `initialize` still omits a
    /// current model. The adapter validates the requested value against that catalogue and
    /// keeps it as a pending selection so the picker does not continue to show the model
    /// from an earlier turn. A later `system/init` report is authoritative and replaces the
    /// pending value with Claude's resolved id.
    #[tokio::test]
    async fn a_successful_model_set_is_visible_before_vendor_echo() {
        let peer = peer("readback").await;
        let refreshed = peer
            .session
            .set_config("model", json!("sonnet"))
            .await
            .unwrap();
        let model = refreshed
            .iter()
            .find(|option| option.id == "model")
            .unwrap();
        assert_eq!(
            model.current_value,
            Some(json!("sonnet")),
            "a successful, catalogue-validated model set must be visible immediately"
        );
        // The write did happen — this is a read-back rule, not a refusal.
        assert_eq!(peer.controls("set_model")[0]["model"], json!("sonnet"));
    }

    /// A later `system/init` is the vendor's authoritative model report. It must
    /// replace the adapter-local pending value and keep the effort picker scoped
    /// to the resolved catalogue entry (including `[1m]` model ids).
    #[tokio::test]
    async fn a_vendor_model_report_replaces_pending_selection_and_keeps_effort() {
        let mut peer = peer_with("model-observed", MODEL_PEER, spec("model-observed"))
            .await
            .expect("the peer initializes");
        let stream = peer.session.events().expect("the stream yields once");

        // The picker must immediately show the selection even though Claude's
        // set_model acknowledgement carries no value.
        peer.session
            .set_config("model", json!("haiku"))
            .await
            .unwrap();
        let pending = peer.session.capabilities().await.unwrap();
        assert_eq!(
            pending.config_option("model").unwrap().current_value,
            Some(json!("haiku"))
        );
        assert!(
            pending.config_option("effort").is_none(),
            "haiku has no effort levels"
        );

        peer.session
            .send(TurnInput {
                text: "first turn".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let _ = peer.user_message().await;

        // Drain enough for the pump to observe system/init. The result itself is
        // not the assertion; this loop waits on the adapter's read-back state.
        for _ in 0..100 {
            let surface = peer.session.capabilities().await.unwrap();
            if surface
                .config_option("model")
                .and_then(|option| option.current_value.clone())
                == Some(json!("sonnet[1m]"))
            {
                assert_eq!(surface.config_option("effort").unwrap().options.len(), 3);
                drop(stream);
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("system/init did not replace the pending model selection");
    }

    /// A prepared Claude process can expose capabilities before it has a native id.  The
    /// first real user turn is the adapter-owned boundary that waits for the vendor's id;
    /// events emitted while that wait is in flight remain on the stream for the caller.
    #[tokio::test]
    async fn an_initial_turn_returns_the_vendor_id_and_keeps_early_events() {
        let mut unnamed = spec("initial-id");
        unnamed.native_id = RequestedNativeId::New;
        let mut peer = peer_with("initial-id", MODEL_PEER, unnamed)
            .await
            .expect("the peer initializes");
        assert!(peer.session.native_id().is_none());
        let stream = peer.session.events().expect("the stream yields once");

        let native = peer
            .session
            .send_initial(TurnInput {
                text: "first turn".into(),
                ..Default::default()
            })
            .await
            .expect("the first turn reports its native id");
        assert_eq!(native.as_str(), "claude-model-observed");
        assert_eq!(
            peer.session.native_id().as_ref().map(NativeId::as_str),
            Some("claude-model-observed")
        );

        let messages = collect(stream, |message| {
            matches!(message, SessionMessage::Event(event)
                if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed))
        })
        .await;
        assert!(
            messages.iter().any(|message| matches!(message,
                SessionMessage::Event(event) if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed))),
            "the result emitted while waiting for the id must remain available"
        );
    }

    /// The same rule where the vendor answers success **with a different value**.
    ///
    /// This is what `manual` does on the real CLI, and this adapter refuses `manual` before
    /// sending it — so the divergence is reproduced on a mode that *is* offered, because the
    /// rule is about reading the confirmation rather than about that one value. The peer
    /// confirms `dontAsk` for a requested `plan`, and the surface must say `dontAsk`.
    #[tokio::test]
    async fn a_vendor_confirming_a_different_value_is_reported_as_the_vendor_said() {
        const NORMALIZES: &str = r#"
log="$1"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"subtype":"initialize"'*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"current_permission_mode":"%s"}}}\n' "$id" "${MODE:-default}"
      ;;
    *'"subtype":"set_permission_mode"'*)
      # Success, confirming something else — exactly what `manual` does on the real CLI.
      MODE=dontAsk
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"mode":"dontAsk"}}}\n' "$id"
      ;;
  esac
done
"#;
        let peer = peer_with("normalizes", NORMALIZES, spec("normalizes"))
            .await
            .unwrap();
        let refreshed = peer
            .session
            .set_config("permissionMode", json!("plan"))
            .await
            .unwrap();
        let mode = refreshed
            .iter()
            .find(|option| option.id == "permissionMode")
            .unwrap();
        assert_eq!(
            mode.current_value,
            Some(json!("dontAsk")),
            "the vendor's confirmation is the authority, not the request"
        );
    }

    /// A turn returns on **acceptance**, and the vendor receives it as a `user` message.
    #[tokio::test]
    async fn a_turn_is_accepted_and_written_as_a_user_message() {
        let mut peer = peer("send").await;
        let stream = peer.session.events().expect("the stream yields once");
        peer.session
            .send(TurnInput {
                text: "hi".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let user = peer.user_message().await;
        assert_eq!(user["message"]["role"], json!("user"));
        assert_eq!(user["message"]["content"], json!("hi"));

        // And the turn's own events arrive, ending in a close — a turn that opened and
        // never closed is the silence the protocol forbids.
        let events = collect(stream, |message| {
            matches!(message, SessionMessage::Event(event)
                if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed))
        })
        .await;
        assert!(
            events
                .iter()
                .any(|message| matches!(message, SessionMessage::Event(event)
                if event.kind == EventKind::Assistant && event.text == "hello")),
            "the assistant's prose must reach the stream: {events:?}"
        );
    }

    /// A prompt command becomes the turn's text, because its output is durable history.
    /// A **control** command never reaches here: it resolves against typed config.
    #[tokio::test]
    async fn a_prompt_command_is_delivered_as_the_turns_text() {
        let peer = peer("command").await;
        let mut args = BTreeMap::new();
        args.insert("target".to_string(), "backend".to_string());
        peer.session
            .send(TurnInput {
                command: Some("verify".into()),
                args,
                ..Default::default()
            })
            .await
            .unwrap();
        let user = peer.user_message().await;
        assert_eq!(user["message"]["content"], json!("/verify backend"));
    }

    /// **A reply is not an event.** A `control_response` must wake its caller and leave the
    /// stream, or a `set_model` acknowledgement appears in the transcript.
    #[tokio::test]
    async fn a_control_reply_never_reaches_the_event_stream() {
        let mut peer = peer("no-leak").await;
        let stream = peer.session.events().unwrap();
        peer.session
            .set_config("model", json!("sonnet"))
            .await
            .unwrap();
        peer.session
            .send(TurnInput {
                text: "hi".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let messages = collect(stream, |message| {
            matches!(message, SessionMessage::Event(event)
                if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed))
        })
        .await;
        for message in &messages {
            if let SessionMessage::Event(event) = message {
                assert_ne!(
                    event.kind,
                    EventKind::Status,
                    "a control reply must not surface as an event: {event:?}"
                );
            }
        }
    }

    /// The stream yields **once**: two consumers of one vendor's stdio would each see half
    /// the turn, and fan-out is the server's job.
    #[tokio::test]
    async fn the_event_stream_is_yielded_once() {
        let mut peer = peer("once").await;
        assert!(peer.session.events().is_some());
        assert!(peer.session.events().is_none());
    }

    /// A vendor request becomes a [`sylph_protocol::Request`], and the answer reaches the
    /// vendor in the shape its kind expects.
    #[tokio::test]
    async fn a_vendor_permission_is_forwarded_and_answered_in_its_own_shape() {
        let mut peer = peer("approve").await;
        let stream = peer.session.events().unwrap();
        peer.session
            .send(TurnInput {
                text: "ASKME".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let request = first_request(stream).await.expect("a request arrives");
        assert_eq!(request.kind, RequestKind::Approval);
        assert_eq!(request.tool.as_deref(), Some("Bash"));

        peer.session
            .resolve(Resolution {
                request_id: request.id.clone(),
                option: "allow".into(),
                input: Some(json!({"command": "ls -la"})),
            })
            .await
            .unwrap();

        let reply = peer.awaited_reply("vendor-1").await;
        assert_eq!(reply["subtype"], json!("success"));
        assert_eq!(reply["response"]["behavior"], json!("allow"));
        // The edited input, which is how "allow, but change the command" works.
        assert_eq!(
            reply["response"]["updatedInput"]["command"],
            json!("ls -la")
        );
    }

    /// **An approval answer carries no `toolName`**, because the CLI's check on it is opt-in
    /// and a wrong value is dropped silently.
    ///
    /// Read from `hasCanUseToolNameMismatch`, which delegates to a helper opening
    /// `if (typeof e !== "string") return false`: absent is no mismatch, so omitting is
    /// accepted, while a value that does not equal the vendor's pending `tool_name` is
    /// ignored with `toolName '…' does not match pending '…'` — and an ignored reply leaves
    /// the request parked, which presents as an approval that vanished. Omitting is therefore
    /// not merely allowed but the safer of the two: an echoed field has to stay equal to the
    /// vendor's name forever, and nothing here would notice if it drifted.
    ///
    /// Pinned as an absence because that is the whole claim. If a later change starts sending
    /// it, this fails and whoever made it has to establish the value is right.
    #[tokio::test]
    async fn an_approval_answer_sends_no_tool_name_field() {
        let mut peer = peer("no-toolname").await;
        let stream = peer.session.events().unwrap();
        peer.session
            .send(TurnInput {
                text: "ASKME".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let request = first_request(stream).await.unwrap();
        peer.session
            .resolve(Resolution {
                request_id: request.id.clone(),
                option: "allow".into(),
                input: None,
            })
            .await
            .unwrap();

        let reply = peer.awaited_reply("vendor-1").await;
        assert_eq!(reply["response"]["behavior"], json!("allow"));
        assert!(
            reply["response"].get("toolName").is_none(),
            "a toolName the vendor can disagree with must not be sent: {}",
            reply["response"]
        );
    }

    /// **Every dialog is answered `success`, never `error`**, including an unrecognized kind.
    ///
    /// `ignoresErrorShapedDialogResponse` ignores an error-shaped reply to a parked
    /// `request_user_dialog` and *leaves the dialog parked* — "not a human choice". So
    /// answering an unknown `dialog_kind` with a coarse accept/reject is not a nicety over
    /// dropping it: an `error` there would park the turn exactly as silence would, and the
    /// `error` path is reachable only for a subtype that is neither `can_use_tool` nor
    /// `request_user_dialog`.
    #[tokio::test]
    async fn an_unknown_dialog_kind_is_answered_success_not_error() {
        const ODD_DIALOG: &str = r#"
log="$1"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"subtype":"initialize"'*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"current_permission_mode":"default"}}}\n' "$id"
      ;;
    *'"type":"user"'*)
      printf '{"type":"control_request","request_id":"vendor-odd","request":{"subtype":"request_user_dialog","dialog_kind":"a_kind_from_a_later_release","payload":{"title":"Proceed?"}}}\n'
      ;;
  esac
done
"#;
        let mut peer = peer_with("odd-dialog", ODD_DIALOG, spec("odd-dialog"))
            .await
            .unwrap();
        let stream = peer.session.events().unwrap();
        peer.session
            .send(TurnInput {
                text: "go".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let request = first_request(stream)
            .await
            .expect("an unknown kind still reaches a client");
        peer.session
            .resolve(Resolution {
                request_id: request.id.clone(),
                option: "allow".into(),
                input: None,
            })
            .await
            .unwrap();

        let reply = peer.awaited_reply("vendor-odd").await;
        assert_eq!(
            reply["subtype"],
            json!("success"),
            "an error-shaped reply to a dialog is ignored and leaves it parked"
        );
        assert_eq!(reply["response"]["behavior"], json!("completed"));
    }

    /// The end-to-end form of the `selectedOptions` defect: a pick has to arrive **in the
    /// field the CLI reads**, over the real wire.
    #[tokio::test]
    async fn a_question_answer_reaches_the_vendor_under_answers() {
        let mut peer = peer("question").await;
        let stream = peer.session.events().unwrap();
        peer.session
            .send(TurnInput {
                text: "QUESTION".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let request = first_request(stream).await.expect("a question arrives");
        assert_eq!(request.kind, RequestKind::Choice);
        peer.session
            .resolve(Resolution {
                request_id: request.id.clone(),
                option: "submit".into(),
                input: Some(json!({"values": {"q:0": "Tabs"}})),
            })
            .await
            .unwrap();

        let reply = peer.awaited_reply("vendor-q").await;
        assert_eq!(
            reply["response"]["updatedInput"]["answers"]["Indentation?"],
            json!("Tabs")
        );
    }

    /// **A request is answered once.** A second resolution must not send a second
    /// `control_response` for an id the CLI has already resolved.
    #[tokio::test]
    async fn a_second_resolution_is_refused_rather_than_sent_twice() {
        let mut peer = peer("twice").await;
        let stream = peer.session.events().unwrap();
        peer.session
            .send(TurnInput {
                text: "ASKME".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let request = first_request(stream).await.unwrap();

        let resolution = Resolution {
            request_id: request.id.clone(),
            option: "allow".into(),
            input: None,
        };
        peer.session.resolve(resolution.clone()).await.unwrap();
        let again = peer.session.resolve(resolution).await;
        assert!(
            again.is_err(),
            "a resolved request has no pending kind left"
        );
        peer.awaited_reply("vendor-1").await;
        assert_eq!(
            peer.replies()
                .iter()
                .filter(|reply| reply["request_id"] == json!("vendor-1"))
                .count(),
            1
        );
    }

    /// An answer to a request this session never raised is refused rather than guessed at.
    ///
    /// The response shape differs per kind and sending the wrong one **parks the request
    /// until its deadline**, which reads as the agent hanging — so a default here would
    /// convert a caller's bug into a stalled turn.
    #[tokio::test]
    async fn resolving_an_unknown_request_is_refused_rather_than_guessed() {
        let peer = peer("unknown-request").await;
        let error = peer
            .session
            .resolve(Resolution {
                request_id: "never-raised".into(),
                option: "allow".into(),
                input: None,
            })
            .await
            .expect_err("nothing was raised");
        assert!(error.to_string().contains("never-raised"));
        assert!(
            peer.replies().is_empty(),
            "nothing may be sent to the vendor"
        );
    }

    /// **A subtype this adapter does not model is answered anyway.**
    ///
    /// A parked request blocks the turn until its deadline, so silence there is worse than
    /// an error the CLI understands.
    #[tokio::test]
    async fn an_unmodelled_vendor_request_is_answered_with_an_error() {
        let mut peer = peer("future").await;
        let _stream = peer.session.events().unwrap();
        peer.session
            .send(TurnInput {
                text: "FUTURE".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let reply = peer.awaited_reply("vendor-f").await;
        assert_eq!(reply["subtype"], json!("error"));
        assert!(reply["error"].as_str().unwrap().contains("some_future_ask"));
    }

    /// `commands_changed` carries only that *something* changed, so the surface is re-read
    /// rather than reconstructed from the notification.
    ///
    /// The re-read is spawned rather than awaited in the pump, because the pump is the only
    /// reader of the reply it would be waiting for — awaiting inline deadlocks the session on
    /// its own `initialize`, and this test is what would catch that.
    #[tokio::test]
    async fn a_commands_changed_notification_causes_a_reread_of_the_surface() {
        let mut peer = peer("changed").await;
        let stream = peer.session.events().unwrap();
        peer.session
            .send(TurnInput {
                text: "CHANGED".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let messages = collect(stream, |message| {
            matches!(message, SessionMessage::Capabilities(_))
        })
        .await;
        let facets = messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Capabilities(facets) => Some(facets),
                _ => None,
            })
            .next_back()
            .expect("a refreshed surface");
        assert_eq!(
            facets.commands.as_ref().map(Vec::len),
            Some(2),
            "the surface must come from a fresh read"
        );
        assert!(peer.controls("initialize").len() >= 2);
    }

    /// `interrupt` ends the turn and keeps the session, which is a control rather than a
    /// signal — conflating the two loses one of two real intentions.
    #[tokio::test]
    async fn interrupt_is_a_control_and_not_a_signal() {
        let peer = peer("interrupt").await;
        peer.session.interrupt().await.unwrap();
        assert_eq!(peer.controls("interrupt").len(), 1);
    }

    /// **Steering is refused, because this vendor has no steer.**
    ///
    /// This test previously asserted that steering wrote a second user message — which it
    /// did, and which was the defect: that write is not read until the turn ends, so it
    /// queued while the composer believed it was correcting. The refusal is
    /// `UNSUPPORTED_FIELD` rather than `INVALID_VALUE` because no value makes this vendor
    /// able to steer, and that code is what lets a client retire the control instead of
    /// leaving it clickable.
    #[tokio::test]
    async fn steering_is_refused_because_this_vendor_has_no_steer() {
        let peer = peer("steer").await;
        let error = match peer.session.steer("stop doing that", None).await {
            Ok(()) => panic!("a vendor with no steer must not accept one"),
            Err(error) => error,
        };
        let refusal = error
            .downcast_ref::<AdapterError>()
            .expect("the refusal travels typed, not as prose");
        assert_eq!(refusal.code, RefusalCode::UnsupportedField);
        assert!(
            refusal.message.contains("queue"),
            "the refusal must name what the caller should do instead: {}",
            refusal.message
        );
    }

    /// Queueing is the second user message on the held-open stdin.
    ///
    /// Same mechanism the old steer used, under the name that says when the agent reads it.
    #[tokio::test]
    async fn queueing_writes_another_user_message() {
        let peer = peer("queue").await;
        peer.session.queue("also check the tests").await.unwrap();
        let user = peer.user_message().await;
        assert_eq!(user["message"]["content"], json!("also check the tests"));
    }

    /// The declared surface matches what the methods do.
    ///
    /// A capability list and its methods are one fact with two derivations, and this vendor
    /// is where they diverged: `Steer` was declared while the method queued. Asserted as a
    /// pair so neither can drift alone.
    #[tokio::test]
    async fn the_declared_capabilities_match_what_the_session_accepts() {
        use sylph_protocol::{Adapter, Capability};
        let info = crate::claude::ClaudeAdapter::new()
            .probe()
            .await
            .expect("claude is installed");
        assert!(
            !info.supports.contains(&Capability::Steer),
            "declaring Steer is what delivered a correction after the thing it prevented"
        );
        assert!(info.supports.contains(&Capability::Queue));
    }

    /// The identity preamble arrives as **text** and this CLI wants a path, so `run` writes
    /// the file and substitutes the marker — a marker rather than an empty string so a
    /// forgotten substitution fails visibly instead of passing `--…-file ""`.
    #[tokio::test]
    async fn the_identity_preamble_is_written_and_its_marker_substituted() {
        let mut injected = spec("soul");
        injected.inject_identity = true;
        injected.identity_preamble = Some("# Sylph\nYou are a sprite.".to_string());
        let peer = peer_with("soul", PEER, injected).await.unwrap();

        let expected = std::env::temp_dir().join(format!(
            "sylph-claude-soul-{}-native-soul.md",
            std::process::id()
        ));
        assert_eq!(
            std::fs::read_to_string(&expected).unwrap(),
            "# Sylph\nYou are a sprite."
        );
        // Idempotent, and it removes the file it wrote.
        peer.session.close().await.unwrap();
        assert!(!expected.exists(), "close removes the preamble it wrote");
    }

    /// `close` is idempotent and escalates unconditionally: a `close` a vendor ignores must
    /// not leak a process for the server's life.
    #[tokio::test]
    async fn close_is_idempotent() {
        let peer = peer("close").await;
        peer.session.close().await.unwrap();
        peer.session.close().await.unwrap();
    }

    /// **A turn open when the vendor dies must not end in silence.**
    ///
    /// A turn with no content and no error says the same thing whether the model failed, the
    /// agent chose not to answer, or the process died — three causes with three different
    /// fixes and one appearance. So the stream carries an `Err` naming the exit before it
    /// ends, which is where `Adapter::run`'s contract puts a failure that needs explaining.
    #[tokio::test]
    async fn a_vendor_dying_mid_turn_explains_itself_before_the_stream_ends() {
        const DIES_MID_TURN: &str = r#"
log="$1"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"subtype":"initialize"'*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"current_permission_mode":"default"}}}\n' "$id"
      ;;
    *'"type":"user"'*)
      printf '{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1","role":"assistant"}}}\n'
      echo 'claude: the model went away' >&2
      exit 1
      ;;
  esac
done
"#;
        let mut peer = peer_with("mid-turn", DIES_MID_TURN, spec("mid-turn"))
            .await
            .unwrap();
        let mut stream = peer.session.events().unwrap();
        peer.session
            .send(TurnInput {
                text: "hi".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        use futures::StreamExt;
        let mut explained = None;
        while let Some(item) = stream.next().await {
            if let Err(error) = item {
                explained = Some(error.to_string());
            }
        }
        let explained = explained.expect("a turn cut short must say so");
        assert!(explained.contains("still running"), "got: {explained}");
        assert!(
            explained.contains("the model went away"),
            "the vendor's own stderr is the diagnosis, got: {explained}"
        );
    }

    /// A control sent after the vendor has gone **fails at once, and says both things**.
    ///
    /// Two assertions, and the timing one is the load-bearing half. The defect this began as
    /// was not a wrong message: a request registering after EOF found a pending map nothing
    /// drains and waited out the full `REPLY_TIMEOUT`, then reported "the vendor did not
    /// answer within 30s" — the wrong sentence for a process that is not running, and 30
    /// seconds paid again on every retry. That is fixed in `Child::request` itself
    /// (`8087085`), so this asserts the *property* rather than the wording: a message can be
    /// reworded, while "fails in well under the timeout" is what a caller depends on.
    ///
    /// The message still has to name which control could not be sent, because "the process
    /// exited" alone leaves the caller knowing the vendor is gone and not what it was doing.
    #[tokio::test]
    async fn a_control_after_the_vendor_exits_fails_at_once_naming_the_control() {
        const ANSWERS_ONCE: &str = r#"
log="$1"
read -r line
printf '%s\n' "$line" >> "$log"
id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"current_permission_mode":"default"}}}\n' "$id"
exit 0
"#;
        let peer = peer_with("once-only", ANSWERS_ONCE, spec("once-only"))
            .await
            .unwrap();
        // Give the process time to exit, so the closure has been observed.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let started = tokio::time::Instant::now();
        let error = peer
            .session
            .interrupt()
            .await
            .expect_err("nothing can answer");
        let waited = started.elapsed();

        assert!(
            waited < Duration::from_secs(5),
            "a dead vendor must fail fast, not wait out the reply timeout; waited {waited:?}"
        );
        let text = error.to_string();
        assert!(
            text.contains("exited") || text.contains("closed"),
            "the error must name the closure, got: {text}"
        );
        assert!(
            text.contains("interrupt"),
            "the error must name which control could not be sent, got: {text}"
        );
    }

    /// Read from the stream until `until` matches, or give up.
    ///
    /// Bounded so a broken adapter fails the test rather than hanging the suite.
    async fn collect(
        mut stream: EventStream,
        until: impl Fn(&SessionMessage) -> bool,
    ) -> Vec<SessionMessage> {
        use futures::StreamExt;
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, stream.next()).await {
                Ok(Some(Ok(message))) => {
                    let done = until(&message);
                    seen.push(message);
                    if done {
                        return seen;
                    }
                }
                Ok(Some(Err(_))) => continue,
                Ok(None) => return seen,
                Err(_) => panic!("the expected message never arrived: {seen:?}"),
            }
        }
    }

    /// The first request the stream carries.
    async fn first_request(stream: EventStream) -> Option<sylph_protocol::Request> {
        collect(stream, |message| {
            matches!(message, SessionMessage::Request(_))
        })
        .await
        .into_iter()
        .find_map(|message| match message {
            SessionMessage::Request(request) => Some(request),
            _ => None,
        })
    }
}
