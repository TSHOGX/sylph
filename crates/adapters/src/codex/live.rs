//! The live `codex` session: `codex app-server` over stdio, held for the session's life.
//!
//! `docs/session-protocol.md` § An adapter is a trait, with three modes governs, and
//! [`super::appserver`] holds the measured facts about the vocabulary this module speaks.
//!
//! # One vocabulary out of three modes, and this is the third producer of it
//!
//! [`super::native`] maps a *stored* item to a [`Event`]; this maps a *live* frame to the
//! same one. Where the two see the same content they emit the same event, deliberately —
//! that is the property `docs/session-protocol.md` calls out and it is why
//! `Translator::frame` is a pure function over a `Value`: the store parser is assertable
//! from a fixture, and a live translator that needed a process to test would be the one
//! half nobody checks.
//!
//! **No name constant is shared with `native`.** The app-server spells an item
//! `commandExecution` and the rollout spells it `CommandExecution`; a shared table would
//! be one vendor rename away from reading a legitimate payload as absent
//! (`super::measured_facts`).
//!
//! # What the live path has that the store does not
//!
//! Exactly one thing, and it is the whole reason `run` is [`sylph_protocol::Fidelity::Stream`]:
//! [`appserver::notification::AGENT_MESSAGE_DELTA`] carries per-token text. Everything
//! else this vendor reports live, it also writes to the rollout — measured at 18 distinct
//! appends over a 26.5 s turn, the first at 0.1 s.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use sylph_protocol::{
    AdapterError, CapabilityFacets, ConfigChoice, ConfigKind, ConfigOption, Event, EventKind,
    EventStream, InteriorState, NativeId, NativeRecord, Phase, Request, RequestKind, RequestOption,
    Resolution, RunSpec, Session, SessionCapabilities, SessionMessage, TurnInput,
};
use tokio::sync::{mpsc, Mutex, Notify};

use super::appserver::{self, method, notification, Decision};
use crate::now_ms;
use crate::process::{self, Child, Line};

/// How long `set_config` waits for the vendor's confirming notification.
///
/// **A timeout here is an expected outcome, not a degradation**, and that is what sets the
/// value. A write that changes nothing emits no notification at all
/// ([`appserver::SETTINGS_REPLY_IS_EMPTY`]), so the fallback is the last known surface rather
/// than a refusal the vendor never made.
///
/// *Measured over every notifying write in
/// `docs/measurements/codex/captures/s04_settings.jsonl`, reply to notification:* **0.7, 1.0,
/// 1.2, 1.9, 3.1, 5.4 ms** — local stdio, no network. 250 ms is 45× the worst observation and
/// still imperceptible on the occasions it does elapse.
///
/// An earlier value of 5 s was ~1000× that maximum, and the reason it was wrong is the no-op
/// rule above rather than the latency: because a no-op notifies nothing, the timeout is the
/// path a user hits *every time they re-select the value already current*. It is the common
/// case, so it cannot be sized for an exceptional one. [`CodexSession::set_config`] now also
/// skips a write that cannot change anything, which removes the wait for that case rather
/// than merely shortening it.
const SETTINGS_ECHO_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// What the translator holds between frames.
///
/// Per-session rather than per-turn, because two of the three facts outlive a turn: the
/// catalogue is fetched once, and an approval raised at the end of one turn can be
/// answered after the next has begun.
#[derive(Default)]
struct State {
    /// The turn `turn/steer` and `turn/interrupt` name.
    ///
    /// Read from the vendor — `turn/start`'s reply and the `turn/started` notification —
    /// never minted here. `turn/steer` refuses a stale one (`expected active turn id X
    /// but found Y`), which is the vendor implementing the precondition
    /// `docs/session-protocol.md` asks for rather than the adapter simulating it.
    turn: Option<String>,
    last_sent_turn: Option<String>,
    /// Approvals raised and not yet answered, keyed by the id a [`Resolution`] echoes.
    ///
    /// The value is the request's own `availableDecisions`, verbatim, because
    /// **that array is the authority** and not the enum
    /// ([`appserver::REQUEST_SCOPED_AUTHORITY`]).
    pending: HashMap<String, Vec<Value>>,
    /// The last `threadSettings` the vendor reported, which is what `capabilities`
    /// answers from.
    ///
    /// **Never what was last set.** `pi`'s adapter reported a field only one code path
    /// populated, so any surface built by another path reported nothing while the session
    /// plainly had a model.
    settings: Option<Value>,
    /// `model/list`'s `data`, fetched once at open.
    ///
    /// Cached because it is the authority for two adapter-side validations the vendor
    /// declines to perform, and it does not change within a session.
    catalogue: Vec<Value>,
    /// This session's own thread id, once the open has reported it.
    ///
    /// Here rather than on [`Translator`] because the read loop is spawned *before* the
    /// open — it has to be, since the open's reply arrives interleaved with notifications
    /// the loop must keep draining. `None` is therefore a real state, and it is exactly
    /// the window in which no frame can be attributed to a thread yet.
    ///
    /// **Per session, not per process.** An earlier draft held this in a module-level
    /// static, which is correct for one session and silently wrong for two: an envagent
    /// holds a session per link on one machine, so the second `run` would overwrite the
    /// first's id and every delegation edge in both sessions would be decided against the
    /// wrong thread — a wrong tree rather than a broken one, which nothing downstream can
    /// detect.
    thread: Option<String>,
    /// Child thread -> the collaboration call that opened it. Codex emits child turn
    /// boundaries on the parent's stdout but no dedicated delegation close record, so this
    /// edge lets the server close the child trajectory instead of treating the boundary as
    /// another root turn event.
    delegation_hosts: HashMap<String, Option<String>>,
    /// Child thread -> the latest name reported by its `subAgentActivity` item.
    delegation_names: HashMap<String, Option<String>>,
}

/// A live codex session.
pub struct CodexSession {
    thread: NativeId,
    child: Arc<Child>,
    state: Arc<Mutex<State>>,
    /// Yielded once by [`Session::events`], because two consumers of one process's stdout
    /// would each see half the turn.
    stream: Mutex<Option<mpsc::Receiver<Result<SessionMessage>>>>,
    /// Raised whenever a [`notification::SETTINGS_UPDATED`] has been folded into
    /// [`State::settings`], so `set_config` can answer from the vendor's confirmation.
    settings_echo: Arc<Notify>,
}

/// Translate vendor frames into session messages, holding the state a frame needs.
///
/// A struct rather than a free function because three frames are only interpretable
/// against earlier ones: a delta needs no state, but a turn's outcome, an approval's
/// answer, and a settings echo all mutate what later calls read.
struct Translator {
    state: Arc<Mutex<State>>,
    settings_echo: Arc<Notify>,
}

impl Translator {
    /// One frame, as zero or more messages.
    ///
    /// Zero is a real and common answer: `thread/status/changed` is the server's own
    /// concept of session status, `account/rateLimits/updated` is billing, and neither is
    /// an event in this vocabulary. Returning a `Vec` rather than an `Option` because one
    /// frame can be two messages — a settings echo is both a capability partial and the
    /// thing `set_config` is waiting on.
    async fn frame(&self, frame: &Value) -> Vec<Result<SessionMessage>> {
        self.frame_at(frame, now_ms()).await
    }

    async fn frame_at(&self, frame: &Value, arrival_at_ms: i64) -> Vec<Result<SessionMessage>> {
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            // A reply with no waiter. Not an error: `initialized` takes no id, and a
            // reply whose caller timed out has nowhere to go.
            return Vec::new();
        };
        let params = frame.get("params").unwrap_or(&Value::Null);
        // **The vendor's own stamp, in epoch milliseconds, and never synthesized when it
        // is there.** `emittedAtMs` is on every notification measured; `now_ms` is the
        // fallback for the one frame kind that carries none, a server *request*.
        let recorded_at_ms = params
            .get("emittedAtMs")
            .or_else(|| frame.get("emittedAtMs"))
            .and_then(Value::as_i64);
        let at_ms = recorded_at_ms.unwrap_or(arrival_at_ms);

        let mut messages = match method {
            notification::ITEM_STARTED => self.item(params, at_ms, Phase::Started).await,
            notification::ITEM_COMPLETED => self.item(params, at_ms, Phase::Completed).await,
            notification::AGENT_MESSAGE_DELTA => {
                delta(params, at_ms, self.thread_span(params).await)
            }
            notification::TURN_STARTED => self.turn_started(params, at_ms).await,
            notification::TURN_COMPLETED => self.turn_completed(params, at_ms).await,
            notification::SETTINGS_UPDATED => self.settings(params).await,
            notification::TOKEN_USAGE => usage(params, at_ms, self.thread_span(params).await),
            notification::ERROR => turn_error(params, at_ms, self.thread_span(params).await),
            notification::WARNING => warning(params, at_ms, self.thread_span(params).await),
            notification::MCP_STARTUP => mcp_startup(params, at_ms, self.thread_span(params).await),
            notification::COMMAND_APPROVAL => self.approval(frame, params).await,
            notification::SERVER_REQUEST_RESOLVED => self.resolved(params).await,
            // **Deliberately dropped, and each one for a stated reason.** An unlisted
            // notification reaching here is dropped too, which is the right default: a
            // frame this build does not model must not become an `Other` event nothing
            // renders, and the vendor declares 153 methods against the handful a turn
            // actually emits.
            //
            // `thread/status/changed` is the *server's* session status, which Sylph owns.
            // `thread/started` repeats `thread/start`'s reply. `account/rateLimits/updated`
            // is billing. `thread/goal/cleared` and `remoteControl/status/changed` concern
            // surfaces this adapter does not drive.
            _ => Vec::new(),
        };
        for message in &mut messages {
            if let Ok(SessionMessage::Event(event)) = message {
                event.recorded_at_ms = recorded_at_ms;
            }
        }
        messages
    }

    /// An `item/*` frame, at the phase its method named.
    async fn item(&self, params: &Value, at_ms: i64, phase: Phase) -> Vec<Result<SessionMessage>> {
        let Some(item) = params.get("item") else {
            return Vec::new();
        };
        let Some(kind) = item.get("type").and_then(Value::as_str) else {
            return Vec::new();
        };
        // **The item stream's own id, which is `id` here and not `call_id`.** The store
        // parser measured these two id spaces disjoint for every item type except the two
        // collaboration ones; live, `id` is the only id an item carries.
        let stream_id = item.get("id").and_then(Value::as_str).map(str::to_string);
        let thread_span = self.thread_span(params).await;

        let event = match kind {
            "agentMessage" => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                // A `started` agentMessage carries `text: ""` and the content arrives as
                // deltas, so emitting it opens the block that the deltas append to. A
                // `completed` carries the whole text, which is what the journal keeps.
                let mut event = Event::new(EventKind::Assistant, Some(phase), at_ms);
                event.stream_id = stream_id;
                event.span = thread_span;
                event.text = text.to_string();
                if let Some(phase_name) = item.get("phase").and_then(Value::as_str) {
                    // The vendor's own `phase` — `final_answer`, `commentary` — which is
                    // a different axis from `sylph_protocol::Phase` and travels in `data`
                    // for exactly that reason. `native.rs` puts it in the same place.
                    event.data = Some(json!({"phase": phase_name}));
                }
                event
            }
            "reasoning" => {
                // **Emitted only when there is plaintext, which live there usually is
                // not.** Measured: every `reasoning` item in the captures carries
                // `summary: []` and `content: []`, matching the store's 4895 items with
                // empty summaries (`native::REASONING_IS_MOSTLY_ENCRYPTED`). A blank
                // `thinking` block in every transcript reads as a rendering bug rather
                // than as an absent record, so an empty one yields nothing.
                let text = flatten_text(item.get("summary")) + &flatten_text(item.get("content"));
                if text.is_empty() {
                    return Vec::new();
                }
                let mut event = Event::new(EventKind::Thinking, Some(phase), at_ms);
                event.stream_id = stream_id;
                event.span = thread_span;
                event.text = text;
                event
            }
            "commandExecution" | "fileChange" | "mcpToolCall" | "imageView" => {
                return tool_item(item, kind, stream_id, thread_span, at_ms, phase)
            }
            "subAgentActivity" => {
                let own = self.state.lock().await.thread.clone();
                let messages = delegation(item, own.as_deref(), at_ms, phase);
                let mut state = self.state.lock().await;
                for message in &messages {
                    if let Ok(SessionMessage::Event(event)) = message {
                        if event.kind == EventKind::Subagent {
                            if let Some(child) = event.stream_id.clone() {
                                state
                                    .delegation_hosts
                                    .insert(child.clone(), event.span.clone());
                                state
                                    .delegation_names
                                    .insert(child.clone(), event.name.clone());
                            }
                        }
                    }
                }
                return messages;
            }
            "collabAgentToolCall" => {
                // **A barrier, not lineage** — `super::measured_facts` item 5. Both
                // real instances carry `receiverThreadIds: []` and `agentsStates: {}`
                // with `tool: "wait"`, and the `status: "completed"` is the *wait's*.
                // Opening a trajectory here would create one for the waiting, with no
                // delegate to put inside it.
                return Vec::new();
            }
            "contextCompaction" => {
                let mut event = Event::new(EventKind::Compact, Some(phase), at_ms);
                event.stream_id = stream_id;
                event.span = thread_span;
                event
            }
            "userMessage" => {
                // Native queue entries carry their client identity. Opening inputs and
                // guarded steers have server receipt events, so their untagged echoes
                // are omitted. Never correlate a native queue by comparing its text.
                if phase != Phase::Completed
                    || item.get("clientId").and_then(Value::as_str).is_none()
                {
                    return Vec::new();
                }
                let mut event = Event::new(EventKind::Other("user".into()), Some(phase), at_ms);
                event.stream_id = stream_id;
                event.span = thread_span;
                event.text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|content| {
                        content
                            .iter()
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                event.data = Some(json!({"vendorMessage": item}));
                event
            }
            // `plan` is reserved by `docs/session-protocol.md` and **no adapter emits it
            // and no timeline branch handles it**, so it would land as `Other("plan")` —
            // a row that draws as nothing. It arrives when its renderer does.
            _ => return Vec::new(),
        };
        vec![Ok(SessionMessage::Event(event))]
    }

    /// Map an app-server frame's emitting thread to the trajectory span used by Sylph.
    ///
    /// The app-server multiplexes child items onto the parent's stdout. Without reading
    /// `params.threadId`, a child's command and assistant blocks become root rows. A missing
    /// thread id remains compatible with older frames and means the root trajectory.
    async fn thread_span(&self, params: &Value) -> Option<String> {
        let thread = params.get("threadId").and_then(Value::as_str);
        let own = self.state.lock().await.thread.clone();
        span_for_thread(thread, own.as_deref())
    }

    /// A `subAgentActivity` item, whose edge direction decides whether it is a delegation
    /// at all.
    ///
    /// **`agentThreadId` is the *other party*, not the child** —
    /// `super::measured_facts` item 4. In a parent's stream it names the delegate; in a
    /// delegate's own stream, `kind: "interacted"` names the **parent**. Mapping it to a
    /// `span` unconditionally inverts the edge, and a reversed pointer is still a
    /// well-formed pointer: the tree it builds is wrong rather than broken.
    ///
    /// Live, this adapter is attached to one thread and knows which it is, so the
    /// direction is decidable — the same input [`super::native::delegation_edge`] needs.
    ///
    /// A free function taking `own` rather than a method, so the decision is assertable
    /// from a fixture without a session — the property that makes the store parser's
    /// equivalent testable.
    async fn turn_started(&self, params: &Value, at_ms: i64) -> Vec<Result<SessionMessage>> {
        let turn = params.get("turn").unwrap_or(&Value::Null);
        let thread = params.get("threadId").and_then(Value::as_str);
        let own = self.state.lock().await.thread.clone();
        if is_child_thread(thread, own.as_deref()) {
            // Child turn boundaries describe the delegated trajectory. They must not become
            // root `turn` events, or the server will treat a child as a second user turn.
            return Vec::new();
        }
        if let Some(id) = turn.get("id").and_then(Value::as_str) {
            let mut state = self.state.lock().await;
            state.last_sent_turn = Some(id.to_string());
            state.turn = Some(id.to_string());
        }
        let mut event = Event::new(EventKind::Turn, Some(Phase::Started), at_ms);
        event.stream_id = turn.get("id").and_then(Value::as_str).map(str::to_string);
        vec![Ok(SessionMessage::Event(event))]
    }
}

/// Reuse both projectors in retained arrival order, with isolated state and no process.
pub(super) async fn reproject_frames(records: &[&NativeRecord]) -> Result<Vec<SessionMessage>> {
    let thread = records.iter().find_map(|record| {
        let params = record.payload.get("params")?;
        params
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .or_else(|| params.get("threadId").and_then(Value::as_str))
            .map(str::to_string)
    });
    let translator = Translator {
        state: Arc::new(Mutex::new(State {
            thread,
            ..State::default()
        })),
        settings_echo: Arc::new(Notify::new()),
    };
    let mut messages = Vec::new();
    let mut native = super::native::RolloutProjector::default();
    for record in records {
        // One session can start as imported history and continue over app-server.
        // Neither family is a session-wide mode: preserve both in first-seen order.
        if !record.source.starts_with("live/") {
            messages.extend(super::native::reproject_native_record(&mut native, record));
            continue;
        }
        for message in translator.frame_at(&record.payload, 0).await {
            let message = message?;
            if matches!(
                message,
                SessionMessage::Event(_) | SessionMessage::TurnInput(_)
            ) {
                messages.push(message);
            }
        }
    }
    Ok(messages)
}

fn span_for_thread(thread: Option<&str>, own: Option<&str>) -> Option<String> {
    match (thread, own) {
        (Some(thread), Some(own)) if thread != own => Some(thread.to_string()),
        _ => None,
    }
}

fn is_child_thread(thread: Option<&str>, own: Option<&str>) -> bool {
    matches!((thread, own), (Some(thread), Some(own)) if thread != own)
}

/// Best-effort summary from a child turn's terminal payload.
fn turn_summary(turn: &Value) -> String {
    turn.get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rev()
        .find_map(|item| {
            (item.get("type").and_then(Value::as_str) == Some("agentMessage"))
                .then(|| item.get("text").and_then(Value::as_str).unwrap_or_default())
        })
        .unwrap_or_default()
        .to_string()
}

fn delegation(
    item: &Value,
    own: Option<&str>,
    at_ms: i64,
    phase: Phase,
) -> Vec<Result<SessionMessage>> {
    {
        // The live payload is camelCase where the store is snake_case, so the store's
        // decision function cannot be called on it. The *rule* is shared; the keys are
        // not (see this module's header).
        let child = item.get("agentThreadId").and_then(Value::as_str);
        let path = item.get("agentPath").and_then(Value::as_str);
        let Some(child) = child else {
            return Vec::new();
        };
        // **Undecidable without our own thread, so nothing is emitted.**
        //
        // `own` is `None` only in the window before the open reply lands, and a
        // `subAgentActivity` cannot legitimately arrive there — a delegation needs a turn,
        // and a turn needs an open thread. But the direction is what decides whether this
        // record is a delegation at all, so guessing it would publish an inverted edge: a
        // reversed pointer is still well-formed, and the tree it builds is wrong rather than
        // broken. Emitting nothing is the honest answer to a question that cannot be
        // answered yet.
        let Some(own) = own else {
            return Vec::new();
        };
        if child == own {
            // Our own thread named as the other party: this is a delegate's record of its
            // parent. Nothing to open.
            return Vec::new();
        }
        // A record whose `agentPath` is the primary agent is the child's view upward.
        if path == Some(super::native::PRIMARY_AGENT_PATH) {
            return Vec::new();
        }

        let mut event = Event::new(EventKind::Subagent, Some(phase), at_ms);
        event.stream_id = Some(child.to_string());
        // The host is the issuing call, which is the item's own `id` — the one item type
        // whose id genuinely *is* a `call_id`.
        event.span = item.get("id").and_then(Value::as_str).map(str::to_string);
        // **The name comes from `agentPath`'s last segment, never from the brief.** From
        // 0.147 the `spawn_agent` brief is ciphertext (`native::BRIEF_IS_CIPHERTEXT`), and
        // a placeholder written in its place becomes this trajectory's durable name —
        // afterwards indistinguishable from one an agent chose.
        event.name = path
            .and_then(|path| path.rsplit('/').next())
            .filter(|segment| !segment.is_empty())
            .map(str::to_string);
        event.interior_state = Some(InteriorState::Partial);
        event.data = Some(json!({
            "kind": item.get("kind").cloned().unwrap_or(Value::Null),
            "agentPath": path,
        }));
        vec![Ok(SessionMessage::Event(event))]
    }
}

impl Translator {
    /// A turn's close, **with the outcome read from the vendor rather than asserted.**
    ///
    /// This is the defect `AGENTS.md` records against `pi`'s adapter, which emitted a
    /// hardcoded `stopReason: "end_turn"` on every settle — so a model that 404'd every
    /// request produced a turn that closed `completed` with no content and no error, and
    /// the Rust port inherited the constant because it compiled and read as a default.
    ///
    /// Measured here, `status` is one of `completed`, `failed`, or `interrupted`, and a
    /// `failed` carries `error.message`. All three are reported as themselves.
    async fn turn_completed(&self, params: &Value, at_ms: i64) -> Vec<Result<SessionMessage>> {
        let turn = params.get("turn").unwrap_or(&Value::Null);
        let thread = params.get("threadId").and_then(Value::as_str);
        let (own, host, name) = {
            let state = self.state.lock().await;
            (
                state.thread.clone(),
                thread
                    .and_then(|id| state.delegation_hosts.get(id).cloned())
                    .flatten(),
                thread
                    .and_then(|id| state.delegation_names.get(id).cloned())
                    .flatten(),
            )
        };

        let status = turn
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("completed");

        if is_child_thread(thread, own.as_deref()) {
            let mut event = Event::new(EventKind::Subagent, Some(Phase::Completed), at_ms);
            event.stream_id = thread.map(str::to_string);
            event.span = host;
            event.name = name;
            event.interior_state = Some(InteriorState::Full);
            event.is_error = status == "failed";
            event.text = turn_summary(turn);
            event.data = Some(json!({
                "stopReason": status,
                "errorMessage": turn.get("error").and_then(|error| error.get("message")),
                "durationMs": turn.get("durationMs"),
                "threadId": thread,
            }));
            event.started_at_ms = turn
                .get("startedAt")
                .and_then(Value::as_i64)
                .map(|seconds| seconds * 1000);
            return vec![Ok(SessionMessage::Event(event))];
        }

        self.state.lock().await.turn = None;
        let mut event = Event::new(EventKind::Turn, Some(Phase::Completed), at_ms);
        event.stream_id = turn.get("id").and_then(Value::as_str).map(str::to_string);
        // **`failed` and `interrupted` are both failures and neither is silence.** A turn
        // with no content and no error says the same thing whether the model refused, the
        // agent chose not to answer, or the adapter dropped the stream.
        event.is_error = status == "failed";
        let message = turn
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str);
        event.data = Some(json!({
            "stopReason": status,
            "errorMessage": message,
            "durationMs": turn.get("durationMs"),
        }));
        // The vendor's own sentence, on the event that reports the outcome. Any path that
        // can end a turn without content has to say why.
        if let Some(message) = message {
            event.text = message.to_string();
        }
        // `startedAt` is **seconds** here, against `emittedAtMs` in milliseconds on the
        // same frame — measured 1787881636 against 1787881647510. Multiplied rather than
        // divided, and by comparison against the sibling stamp rather than by eyeballing
        // the magnitude, which is the check `AGENTS.md` requires after a sibling vendor's
        // store decoded to 1970.
        event.started_at_ms = turn
            .get("startedAt")
            .and_then(Value::as_i64)
            .map(|seconds| seconds * 1000);
        vec![Ok(SessionMessage::Event(event))]
    }

    /// The vendor's confirmation of the config surface.
    async fn settings(&self, params: &Value) -> Vec<Result<SessionMessage>> {
        let Some(settings) = params.get("threadSettings") else {
            return Vec::new();
        };
        let options = {
            let mut state = self.state.lock().await;
            state.settings = Some(settings.clone());
            config_options(settings, &state.catalogue)
        };
        // Woken *after* the surface is stored, so a `set_config` that returns on this
        // signal reads the new value rather than racing it.
        self.settings_echo.notify_waiters();
        // **Facet-scoped**: `config` only. An unmentioned facet cannot be cleared, which
        // is the rule `claude`'s `system/init` broke by answering a whole-surface payload
        // carrying only the permission mode, erasing 46 slash commands per session.
        vec![Ok(SessionMessage::Capabilities(CapabilityFacets::config(
            options,
        )))]
    }

    /// An approval, which is a JSON-RPC **request** and therefore carries an `id` this
    /// adapter must answer.
    async fn approval(&self, frame: &Value, params: &Value) -> Vec<Result<SessionMessage>> {
        // The id is on the frame, as JSON-RPC puts it, and it is a *number* — `0` on the
        // first request of a session. Kept as its decimal spelling because a
        // `Resolution.request_id` is a string.
        let Some(id) = frame.get("id").map(reply_key) else {
            return vec![Err(anyhow!(
                "codex raised an approval with no id, which cannot be answered"
            ))];
        };
        let available = params
            .get("availableDecisions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // **The request's own offer is the authority, so it is what is remembered.** A
        // global allowlist fails in both directions at once: every measured approval
        // offered an object variant and none offered `acceptForSession`.
        self.state
            .lock()
            .await
            .pending
            .insert(id.clone(), available.clone());

        let command = params.get("command").and_then(Value::as_str);
        let mut request = Request {
            id,
            kind: RequestKind::Approval,
            // Pairs the dialog to the `tool_call` already on the timeline, which is what
            // keeps it from floating beside the call it is about.
            tool_use_id: params
                .get("itemId")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool: Some("exec".to_string()),
            title: match command {
                Some(command) => format!("Run {command}"),
                None => "Run a command".to_string(),
            },
            detail: Some(json!({
                "command": command,
                "cwd": params.get("cwd"),
                "commandActions": params.get("commandActions"),
            })),
            options: available.iter().filter_map(decision_option).collect(),
            // **`None` is correct for an approval**, whose subject *is* one proposed tool
            // input — a client renders it from `detail`. A `Form` here would be the
            // client sniffing `detail` for field names, which is the defect
            // `sylph_protocol::Form` exists to close.
            form: None,
            default: None,
            // Not marked destructive: this vendor offers no risk signal, and inferring
            // one from the command text would be a prose test against a string the user
            // wrote. Absent means the client decides how to present it.
            risk: None,
            allow_persistent: available.iter().any(is_persistent_decision),
            expires_at_ms: None,
        };
        // A vendor that offered nothing leaves a dialog with no answer, which would
        // strand the turn. Reported rather than rendered.
        if request.options.is_empty() {
            return vec![Err(anyhow!(
                "codex offered no decisions for approval {}, so it cannot be answered",
                request.id
            ))];
        }
        request.detail = request.detail.filter(|detail| !detail.is_null());
        vec![Ok(SessionMessage::Request(request))]
    }

    /// A request somebody else answered.
    ///
    /// **Measured: approvals fan out to every subscriber, and any of them may answer.** So
    /// this is not merely bookkeeping — forgetting the row is what lets [`Session::resolve`]
    /// refuse a late answer with a sentence rather than sending a decision the vendor has
    /// stopped waiting for.
    async fn resolved(&self, params: &Value) -> Vec<Result<SessionMessage>> {
        if let Some(id) = params.get("requestId").map(reply_key) {
            self.state.lock().await.pending.remove(&id);
        }
        // Nothing is emitted. The server owns retiring a request it published, and
        // `docs/session-protocol.md` gives an adapter no message for "withdraw the ask" —
        // see this module's finding in `CodexSession::resolve`.
        Vec::new()
    }
}

/// A per-token delta, the one thing the live path has that the store does not.
fn delta(params: &Value, at_ms: i64, span: Option<String>) -> Vec<Result<SessionMessage>> {
    let Some(text) = params.get("delta").and_then(Value::as_str) else {
        return Vec::new();
    };
    let mut event = Event::new(EventKind::Assistant, Some(Phase::Delta), at_ms);
    // **`itemId` here, where the item frames say `id`.** The same block under two key
    // names in one vocabulary; reading `id` off a delta yields no stream id, so every
    // delta would open its own block and the message would render as one word per row.
    event.stream_id = params
        .get("itemId")
        .and_then(Value::as_str)
        .map(str::to_string);
    event.span = span;
    event.text = text.to_string();
    vec![Ok(SessionMessage::Event(event))]
}

/// A tool item, which live carries both the call and its outcome.
///
/// The store parser has to split these — a rollout records a call's arguments in one
/// stream and its status in another — while here one item carries `command`, `status`,
/// `exitCode`, and `aggregatedOutput` together. So a `started` item is the call and a
/// `completed` item is the result, which is what makes the pair render as one block.
fn tool_item(
    item: &Value,
    kind: &str,
    stream_id: Option<String>,
    span: Option<String>,
    at_ms: i64,
    phase: Phase,
) -> Vec<Result<SessionMessage>> {
    let name = match kind {
        "fileChange" => "apply_patch",
        "mcpToolCall" => "mcp",
        "imageView" => "view_image",
        _ => "exec",
    };
    // A `completed` item is the outcome; anything earlier is the call. `Phase::Completed`
    // on a `tool_call` would mean "its arguments finished streaming", which is not what
    // this frame reports.
    let (event_kind, event_phase) = match phase {
        Phase::Completed => (EventKind::ToolResult, Phase::Completed),
        other => (EventKind::ToolCall, other),
    };
    let mut event = Event::new(event_kind, Some(event_phase), at_ms);
    event.stream_id = stream_id;
    event.span = span;
    event.name = Some(name.to_string());
    // **`status` is the discriminant, and an exit code is not enough on its own.** A
    // `fileChange` and an `mcpToolCall` carry no exit code at all, so reading only that
    // reports every failed MCP call as a success — measured in the store at 12 of 18.
    event.is_error = item.get("status").and_then(Value::as_str) == Some("failed");
    if event.kind == EventKind::ToolResult {
        event.text = item
            .get("aggregatedOutput")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    } else {
        event.text = item
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }
    let mut detail = json!({});
    if let Some(object) = detail.as_object_mut() {
        for field in [
            "command",
            "cwd",
            "exitCode",
            "durationMs",
            "status",
            "changes",
        ] {
            if let Some(value) = item.get(field).filter(|value| !value.is_null()) {
                object.insert(field.to_string(), value.clone());
            }
        }
    }
    event.data = Some(detail);
    vec![Ok(SessionMessage::Event(event))]
}

fn usage(params: &Value, at_ms: i64, span: Option<String>) -> Vec<Result<SessionMessage>> {
    let Some(usage) = params.get("tokenUsage") else {
        return Vec::new();
    };
    let mut event = Event::new(EventKind::Usage, Some(Phase::Completed), at_ms);
    event.span = span;
    event.data = Some(usage.clone());
    vec![Ok(SessionMessage::Event(event))]
}

/// A turn-level error, which is the vendor telling us why a turn will produce nothing.
///
/// **This must reach the record.** Measured on `pi`, a model whose testing period had
/// ended answered 404 on every request and the observable result in Sylph was a turn that
/// closed clean and said nothing — three causes with three different fixes and one
/// appearance. `EventKind::Notice` is where the vendor's sentence goes.
fn turn_error(params: &Value, at_ms: i64, span: Option<String>) -> Vec<Result<SessionMessage>> {
    let error = params.get("error").unwrap_or(&Value::Null);
    let mut event = Event::new(EventKind::Notice, None, at_ms);
    event.span = span;
    event.is_error = true;
    event.text = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("codex reported an error with no message")
        .to_string();
    event.data = Some(json!({
        "codexErrorInfo": error.get("codexErrorInfo"),
        // Whether the vendor will retry on its own. An adapter must not retry a turn
        // itself, so this is reported rather than acted on.
        "willRetry": params.get("willRetry"),
    }));
    vec![Ok(SessionMessage::Event(event))]
}

/// A `warning`, which is the **only** signal that a model name was never real.
///
/// Measured: `thread/settings/update` accepts `no-such-model-xyz`, echoes it back as the
/// current value, and the sole evidence is this notification — `Model metadata for
/// 'no-such-model-xyz' not found. Defaulting to fallback metadata`. Dropping it would
/// leave a session running a fallback model while the picker showed a checkmark on a model
/// that does not exist.
fn warning(params: &Value, at_ms: i64, span: Option<String>) -> Vec<Result<SessionMessage>> {
    let Some(message) = params.get("message").and_then(Value::as_str) else {
        return Vec::new();
    };
    let mut event = Event::new(EventKind::Notice, None, at_ms);
    event.span = span;
    event.text = message.to_string();
    vec![Ok(SessionMessage::Event(event))]
}

/// An MCP server's startup, which is worth surfacing only when it failed.
///
/// A `starting` then `ready` pair per server per session would put two status rows in
/// every transcript for work the user did not ask about. A failure is different: an MCP
/// server that did not come up removes tools the agent would otherwise have, and nothing
/// else reports it.
fn mcp_startup(params: &Value, at_ms: i64, span: Option<String>) -> Vec<Result<SessionMessage>> {
    let failed = params.get("error").is_some_and(|value| !value.is_null())
        || params
            .get("failureReason")
            .is_some_and(|value| !value.is_null());
    if !failed {
        return Vec::new();
    }
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("an MCP server");
    let mut event = Event::new(EventKind::Notice, None, at_ms);
    event.span = span;
    event.is_error = true;
    event.text = format!("{name} failed to start");
    event.data = Some(json!({
        "name": name,
        "error": params.get("error"),
        "failureReason": params.get("failureReason"),
    }));
    vec![Ok(SessionMessage::Event(event))]
}

/// One offered decision as a [`RequestOption`] a client can render.
///
/// The option's `id` is the wire token for a string decision and the **member name** for
/// an object one, which is what [`Session::resolve`] maps back through
/// [`appserver::is_offered`]. The payload is not carried in the id: it is echoed from the
/// remembered offer, because rebuilding an amendment invites one that does not match what
/// the user was shown.
fn decision_option(offered: &Value) -> Option<RequestOption> {
    let (id, label, persistent) = match offered {
        Value::String(token) => (
            token.clone(),
            match token.as_str() {
                "accept" => "Allow once",
                "acceptForSession" => "Allow for this session",
                "decline" => "Deny",
                // Measured: `decline` lets the agent continue, `cancel` interrupts the
                // turn. Two different intentions, so two different labels.
                "cancel" => "Deny and stop the turn",
                other => other,
            }
            .to_string(),
            token == "acceptForSession",
        ),
        Value::Object(map) => {
            let name = map.keys().next()?.clone();
            let label = match name.as_str() {
                "acceptWithExecpolicyAmendment" => "Allow, and stop asking for commands like this",
                "applyNetworkPolicyAmendment" => "Allow, and remember this host",
                other => other,
            }
            .to_string();
            (name, label, true)
        }
        _ => return None,
    };
    Some(RequestOption {
        id,
        label,
        persistent,
        description: None,
    })
}

/// Whether an offered decision would write a lasting grant.
fn is_persistent_decision(offered: &Value) -> bool {
    match offered {
        Value::String(token) => token == "acceptForSession",
        Value::Object(_) => true,
        _ => false,
    }
}

/// The config surface, built from the vendor's reported settings.
///
/// **Every `current_value` comes from `settings`, which is the vendor's confirmation** —
/// never from a request. And the sandbox value is translated back through
/// [`appserver::sandbox_mode_from_policy`], because the vendor reports `workspaceWrite`
/// where a client sends `workspace-write` and an untranslated read-back matches no offered
/// row.
fn config_options(settings: &Value, catalogue: &[Value]) -> Vec<ConfigOption> {
    let string_at = |key: &str| {
        settings
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let mut options = Vec::new();

    let model = string_at("model");
    options.push(ConfigOption {
        id: "model".into(),
        name: "Model".into(),
        description: None,
        category: Some("model".into()),
        kind: ConfigKind::Select,
        current_value: model.clone().map(Value::String),
        options: catalogue
            .iter()
            .filter(|entry| entry.get("hidden").and_then(Value::as_bool) != Some(true))
            .filter_map(|entry| {
                let id = entry.get("id").and_then(Value::as_str)?;
                Some(ConfigChoice {
                    // **The bare id, and it is unique here.** `pi` needed a
                    // provider-qualified value because two providers offered one id; this
                    // vendor's catalogue is one provider's, and `checked` is computed as
                    // a filter — so a duplicate would put a checkmark on two rows.
                    value: id.to_string(),
                    name: entry
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or(id)
                        .to_string(),
                    description: entry
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect(),
    });

    // **Per model, because that is how the vendor reports it.** A session-global effort
    // enumeration is wrong for this vendor and for `claude`.
    let efforts = model
        .as_deref()
        .and_then(|model| appserver::supported_efforts(catalogue, model))
        // A model the catalogue does not hold — which a bogus `model` write produces —
        // leaves the ladder to the spawn-time list rather than empty, so the control stays
        // usable while the warning explains the model.
        .unwrap_or_else(|| {
            super::SPAWN_EFFORT_CHOICES
                .iter()
                .map(|e| e.to_string())
                .collect()
        });
    options.push(ConfigOption {
        id: "effort".into(),
        name: "Reasoning effort".into(),
        description: None,
        category: Some("effort".into()),
        kind: ConfigKind::Select,
        current_value: string_at("effort").map(Value::String),
        options: efforts
            .into_iter()
            .map(|value| ConfigChoice {
                name: value.clone(),
                value,
                description: None,
            })
            .collect(),
    });

    options.push(select(
        "approvalPolicy",
        "Approval policy",
        settings.get("approvalPolicy").cloned(),
        super::APPROVAL_POLICIES,
    ));
    options.push(select(
        "sandbox",
        "Sandbox",
        settings
            .get("sandboxPolicy")
            .and_then(appserver::sandbox_mode_from_policy)
            .map(|mode| Value::String(mode.to_string())),
        super::SANDBOX_MODES,
    ));
    options.push(select(
        "personality",
        "Personality",
        settings.get("personality").cloned(),
        super::PERSONALITIES,
    ));
    options
}

fn select(id: &str, name: &str, current: Option<Value>, choices: &[&str]) -> ConfigOption {
    ConfigOption {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        category: Some(id.to_string()),
        kind: ConfigKind::Select,
        current_value: current.filter(|value| !value.is_null()),
        options: choices
            .iter()
            .map(|value| ConfigChoice {
                value: value.to_string(),
                name: value.to_string(),
                description: None,
            })
            .collect(),
    }
}

/// Flatten a content array's `text` members.
fn flatten_text(content: Option<&Value>) -> String {
    let Some(items) = content.and_then(Value::as_array) else {
        return String::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::String(text) => Some(text.clone()),
            // Read by **presence of `text`** rather than by matching a `type` tag: this
            // vendor spells the same block `text`, `Text`, and `output_text` in its three
            // vocabularies, and a fourth casing would silently empty every message.
            other => other
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
        .collect::<Vec<_>>()
        .join("")
}

/// A JSON-RPC id as the string a [`Resolution`] carries.
///
/// A number here and a string there, and the decimal spelling is what makes one map serve
/// both — the same reason `process::reply_id` exists.
fn reply_key(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A scripted peer, for the tests below.
///
/// `codex app-server` speaks NDJSON JSON-RPC over stdio, so a shell script that reads a
/// line and prints frames is a faithful peer — the same technique `process.rs` tests itself
/// with, and the reason none of these tests needs the real CLI. **A test that needs the
/// vendor installed is a test that does not run.**
#[cfg(test)]
fn scripted_peer(script: &str) -> process::Spawn {
    process::Spawn {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        cwd: "/tmp".to_string(),
        env: HashMap::new(),
        env_remove: Vec::new(),
        // The same predicate the real spawn uses, so these tests exercise the correlation
        // the session actually runs with rather than a simplified stand-in.
        reply_id: Some(Box::new(process::json_rpc_reply_id)),
    }
}

/// Open a live session: start the app-server, handshake, and open or resume the thread.
///
/// # An open that reports no id is a failure
///
/// `codex` is the one supported agent that assigns its own session id
/// (`dictates_native_id: false`), so this function's most important job is to report the
/// id the *vendor* chose. `sylph_protocol::AgentInfo::dictates_native_id` states the
/// consequence of getting it wrong: without an id the session can never be resumed, so
/// every link death — the idle reaper, a restart, an explicit detach — would make it
/// permanently unlinkable. So a missing `result.thread.id` is an error here and never a
/// synthesized value.
pub async fn run(spec: RunSpec) -> Result<Box<dyn Session>> {
    // Decided before the process starts, so a dictated id is refused without having spawned
    // anything.
    appserver::thread_start_params(&spec)?;

    let (child, lines) = Child::spawn(process::Spawn {
        program: super::AGENT_NAME.to_string(),
        // **`app-server` alone, and no `--listen`.** See this module's header: the socket
        // is WebSocket-framed and exists for attaching to a daemon somebody else started.
        args: vec!["app-server".to_string()],
        cwd: spec.workdir.clone(),
        env: HashMap::new(),
        env_remove: Vec::new(),
        // **The predicate, not a field name.** codex numbers its *server* requests in the
        // same space `next_request_id` mints from — measured at 0,1,2 against our 1.. — so a
        // reader keyed on the presence of an `id` would route an approval into whichever
        // caller was waiting, and the turn would block forever on a human nobody asked.
        // `json_rpc_reply_id` answers `None` for any frame carrying `method`, which is
        // JSON-RPC's own discriminant between a reply and a request.
        reply_id: Some(Box::new(process::json_rpc_reply_id)),
    })
    .await?;
    Ok(Box::new(open(child, lines, spec).await?))
}

/// Handshake and open a thread on an already-spawned peer.
///
/// Split from [`run`] so the whole open sequence is exercised against a **scripted JSON-RPC
/// peer** rather than the installed CLI. That is the same reason `argv_for` is pure: a test
/// that needs the vendor installed is a test that does not run, and the open is where three
/// of this vendor's measured hazards live — the `experimentalApi` gate, the id it assigns
/// itself, and the two different spellings its settings arrive under.
async fn open(child: Child, lines: mpsc::Receiver<Line>, spec: RunSpec) -> Result<CodexSession> {
    let (open_method, open_params) = appserver::thread_start_params(&spec)?;
    let child = Arc::new(child);
    let state = Arc::new(Mutex::new(State::default()));
    let settings_echo = Arc::new(Notify::new());

    // The channel the server reads. Bounded, and the read loop is the only producer.
    let (tx, rx) = mpsc::channel::<Result<SessionMessage>>(512);

    // Started before the handshake, because a reply cannot be awaited until something is
    // draining stdout — `thread/started` and `mcpServer/startupStatus/updated` arrive
    // interleaved with `thread/start`'s own reply.
    tokio::spawn(read_loop(
        lines,
        tx,
        state.clone(),
        settings_echo.clone(),
        child.clone(),
        format!("live/stdout/{}.jsonl", uuid::Uuid::new_v4()),
    ));

    // **`experimentalApi` is declared, and it is not optional.** Measured: without it,
    // `collaborationMode/list` and `remoteControl/status/read` are refused `-32600 …
    // requires experimentalApi capability`. Every probe that started a thread declared it.
    request(
        &child,
        method::INITIALIZE,
        json!({
            "clientInfo": {"name": "sylph", "title": "Sylph", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": true},
        }),
    )
    .await
    .context("codex refused the app-server handshake")?;
    // A notification, so no id and nothing to await. Sent before the thread opens, as the
    // vendor's own flow does.
    child
        .send(json!({"jsonrpc": "2.0", "method": method::INITIALIZED, "params": {}}))
        .await?;

    // The catalogue, before the thread, because it is what validates a `model` or `effort`
    // write and the vendor validates neither.
    let catalogue = request(&child, method::MODEL_LIST, json!({}))
        .await
        .ok()
        .and_then(|reply| {
            reply
                .get("result")
                .and_then(|result| result.get("data"))
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default();

    let opened = request(&child, open_method, open_params)
        .await
        .with_context(|| {
            format!(
                "codex refused {open_method}{}",
                if open_method == method::THREAD_RESUME {
                    "; a thread with no completed turn is unresumable \
                     (`no rollout found for thread id`)"
                } else {
                    ""
                }
            )
        })?;
    let result = opened
        .get("result")
        .ok_or_else(|| anyhow!("codex answered {open_method} without a result: {opened}"))?;
    let thread = result
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
        // **Empty is absent.** `NativeId::new` is deliberately infallible and unvalidated —
        // a native id is the vendor's, and refusing one for an inconvenient character would
        // make a real session unrepresentable — so the check that an id was actually
        // *reported* has to happen here. An empty string would otherwise construct a
        // perfectly valid `NativeId` that names nothing, which is the degraded success
        // `AgentInfo::dictates_native_id` forbids.
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "codex opened a thread without reporting its id at result.thread.id, so \
                 the session could never be resumed: {result}"
            )
        })?;
    let thread = NativeId::new(thread);

    // **The opening settings come from the open reply, not from what was asked for.** The
    // reply carries `model`, `reasoningEffort`, `approvalPolicy`, and `sandbox` at its top
    // level, under *different names* from the ones `thread/settings/updated` uses — so they
    // are folded into that shape here, in one place, rather than every reader knowing both.
    {
        let mut held = state.lock().await;
        held.catalogue = catalogue;
        held.settings = Some(settings_from_open(result));
        // Published last, because it is what makes the read loop start attributing frames:
        // until the thread is known a `subAgentActivity` cannot have its edge direction
        // decided, and a guessed direction is a wrong tree rather than a broken one.
        held.thread = Some(thread.as_str().to_string());
    }

    Ok(CodexSession {
        thread,
        child,
        state,
        stream: Mutex::new(Some(rx)),
        settings_echo,
    })
}

/// The open reply's settings, in the shape `thread/settings/updated` reports.
///
/// **The two payloads name the same facts differently**: the open reply says
/// `reasoningEffort` and `sandbox`, while the notification says `effort` and
/// `sandboxPolicy`. Measured on both. Normalized here so `config_options` reads one shape —
/// two readers would be two chances for a rename to empty half the surface.
fn settings_from_open(result: &Value) -> Value {
    json!({
        "model": result.get("model"),
        "effort": result.get("reasoningEffort"),
        "approvalPolicy": result.get("approvalPolicy"),
        "sandboxPolicy": result.get("sandbox"),
        "personality": result.get("personality"),
        "cwd": result.get("cwd"),
    })
}

#[async_trait]
impl Session for CodexSession {
    fn native_id(&self) -> Option<NativeId> {
        Some(self.thread.clone())
    }

    /// The surface the **vendor** last reported.
    ///
    /// Built from `State::settings`, which only ever holds a payload codex sent — the
    /// open reply or a `thread/settings/updated`. Never from what was last set: a session
    /// can also be steered by the agent's own TUI, and the vendor is the only thing that
    /// knows whether a write was honoured.
    async fn capabilities(&self) -> Result<SessionCapabilities> {
        let state = self.state.lock().await;
        Ok(SessionCapabilities {
            // **Empty, and a client must not present that as a failure.** codex has no
            // command concept at all, so there is nothing to reach and nothing missing
            // (`super` module header; `sylph_protocol::LinkFacets`).
            commands: Vec::new(),
            // Delegates exist and are discovered from `subAgentActivity` as a turn runs;
            // there is no method that enumerates them in advance, and a guessed list would
            // be a claim about the session.
            subagents: Vec::new(),
            config: state
                .settings
                .as_ref()
                .map(|settings| config_options(settings, &state.catalogue))
                .unwrap_or_default(),
            // codex spells this `approvalPolicy` and it travels as a config option above.
            // Reporting it here as well would be one fact with two derivations, which
            // diverge the moment either grows a case.
            permission_mode: None,
            context_window: None,
        })
    }

    /// Start a turn. Returns once codex has **accepted** it.
    async fn send(&self, turn: TurnInput) -> Result<()> {
        let mut input: Vec<Value> = Vec::new();
        // A `Prompt` command becomes prompt text, because codex has no command concept to
        // route it to. Dropping it would silently lose what the user typed.
        let text = match &turn.command {
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
        if !text.is_empty() {
            input.push(json!({"type": "text", "text": text}));
        }
        for path in &turn.attachments {
            // **`localImage` with a `path`, not `image` with a `url`.** The schema declares
            // both, and the vendor runs on the machine holding the file — sending a local
            // path as a `url` would ask it to fetch `file:///…` through an image loader.
            input.push(json!({"type": "localImage", "path": path}));
        }
        if input.is_empty() {
            return Err(anyhow!("a codex turn needs text or an attachment"));
        }

        let mut params = json!({"threadId": self.thread.as_str(), "input": input});
        let object = params.as_object_mut().expect("built as an object");
        // Per-turn overrides travel **with** the turn, which is why `turn/start` takes them:
        // sending them as preceding calls leaves a window where a turn runs under a mix of
        // old and new settings.
        for (id, value) in &turn.config {
            match id.as_str() {
                "sandbox" => {
                    if let Some(policy) = value.as_str().and_then(appserver::sandbox_policy) {
                        object.insert("sandboxPolicy".into(), policy);
                    }
                }
                "model" | "effort" | "approvalPolicy" | "personality" => {
                    object.insert(id.clone(), value.clone());
                }
                // An unknown per-turn override is dropped rather than forwarded: the vendor
                // accepts an unknown option name silently, so forwarding it would report an
                // override that never applied.
                _ => {}
            }
        }

        let reply = request(&self.child, method::TURN_START, params).await?;
        // **The turn id is read from the reply, not awaited on the notification.** A steer
        // sent immediately after `send` needs `expectedTurnId`, and `turn/started` arrives
        // 13 ms later — measured. Waiting for the notification would make a fast client's
        // steer fail its own precondition.
        let id = reply
            .get("result")
            .and_then(|result| result.get("turn"))
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "codex accepted turn/start without a turn identity; delivery is unconfirmed"
                )
            })?;
        let mut state = self.state.lock().await;
        state.last_sent_turn = Some(id.to_string());
        state.turn = Some(id.to_string());
        Ok(())
    }

    async fn last_sent_turn_id(&self) -> Option<String> {
        self.state.lock().await.last_sent_turn.clone()
    }

    /// Start a prepared link's first turn.  Codex assigns a thread id in
    /// `thread/start`, before any turn exists, so sending the prompt does not need
    /// an additional identity wait here.
    async fn send_initial(&self, turn: TurnInput) -> Result<NativeId> {
        self.send(turn).await?;
        Ok(self.thread.clone())
    }

    /// End the running turn, keeping the session.
    async fn interrupt(&self) -> Result<()> {
        // The vendor reader may already know a successor while the envagent is still
        // closing our lease. Keep the id captured by send rather than redirecting this
        // interrupt to whichever turn most recently appeared on the vendor stream.
        let turn = {
            let state = self.state.lock().await;
            state.last_sent_turn.clone().or_else(|| state.turn.clone())
        }
        .ok_or_else(|| {
            // Not an `AdapterError`: `Capability::Interrupt` *is* offered, so this is not a
            // refusal about the control. It is "there is nothing running", which must stay
            // distinguishable — a client disabling its stop button over this would lose it
            // for the session's life.
            anyhow!("no codex turn is running, so there is nothing to interrupt")
        })?;
        request(
            &self.child,
            method::TURN_INTERRUPT,
            json!({"threadId": self.thread.as_str(), "turnId": turn}),
        )
        .await?;
        Ok(())
    }

    /// Add text to a turn already running.
    ///
    /// **`expectedTurnId` is required and the vendor enforces it** — `expected active turn
    /// id X but found Y`. That is the mid-turn precondition `docs/session-protocol.md`
    /// asks for, implemented by the vendor rather than simulated here, so a steer racing a
    /// turn boundary is refused instead of landing in the next conversation.
    async fn steer(&self, text: &str, expected_turn_id: Option<&str>) -> Result<()> {
        let turn =
            match expected_turn_id {
                Some(id) => id.to_string(),
                // `None` means "whatever is running", which is a coherent ask from a client
                // with no turn in view. The adapter substitutes the turn it last saw the vendor
                // report — still the vendor's value, so the precondition remains real.
                None => self.state.lock().await.turn.clone().ok_or_else(|| {
                    anyhow!("no codex turn is running, so there is nothing to steer")
                })?,
            };
        request(
            &self.child,
            method::TURN_STEER,
            json!({
                "threadId": self.thread.as_str(),
                "expectedTurnId": turn,
                "input": [{"type": "text", "text": text}],
            }),
        )
        .await?;
        Ok(())
    }

    /// Queue text for after the running turn: `thread/queue/add`.
    ///
    /// **`clientUserMessageId` is required and undocumented.** *Measured in
    /// `docs/measurements/codex/captures/s03_control.jsonl`:* a `queue/add` carrying only
    /// `threadId` and `input` is refused `-32600 Invalid request: missing field
    /// `clientUserMessageId``. That is the good case for this vendor — it is one of the
    /// enum-shaped refusals rather than one of the silent accepts — but the field appears in
    /// no documentation, so the value is ours to choose.
    ///
    /// A fresh id per call, from the transport's own counter, rather than a hash of the text:
    /// queueing the same sentence twice is a real thing to want, and an id derived from
    /// content would make the second one look like a duplicate of the first to a vendor that
    /// dedupes on it. `thread/queue/list` keys on this id, so it is also what a future
    /// "unqueue that" would name.
    ///
    /// No turn-open precondition, unlike [`Self::steer`]: the queue is a property of the
    /// thread rather than of a turn, and adding to it with nothing running is what makes the
    /// next turn's opening text.
    async fn queue(&self, text: &str) -> Result<()> {
        request(
            &self.child,
            method::THREAD_QUEUE_ADD,
            json!({
                "threadId": self.thread.as_str(),
                "clientUserMessageId": format!("sylph-{}", self.child.next_request_id()),
                "input": [{"type": "text", "text": text}],
            }),
        )
        .await?;
        Ok(())
    }

    /// Set one option, answering with the **refreshed array**.
    ///
    /// # Validation lives here for three of the five, because the vendor declines it
    ///
    /// Measured on 0.150.1 (`appserver::refuse_unknown` carries the table): an unknown
    /// option name is accepted silently with `result: {}`, a bogus `effort` is accepted and
    /// echoed back as current, and a nonexistent model behaves the same. Only the
    /// enum-typed fields refuse a bad value. So anything not enum-typed is checked against
    /// what the vendor enumerated, or a refused write is indistinguishable from an applied
    /// one.
    ///
    /// The two codes are not interchangeable: no such option is `UnsupportedField` and
    /// justifies disabling the control, while a bad value is `InvalidValue` and must leave
    /// it usable.
    async fn set_config(&self, id: &str, value: Value) -> Result<Vec<ConfigOption>> {
        let (catalogue, model) = {
            let state = self.state.lock().await;
            (
                state.catalogue.clone(),
                state
                    .settings
                    .as_ref()
                    .and_then(|settings| settings.get("model"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        };
        let text = value.as_str().map(str::to_string);
        let named = |value: &Value| -> String { value.to_string() };

        // The wire member and the validated value, per option.
        let (key, wire) = match id {
            "model" => {
                let requested = text
                    .clone()
                    .ok_or_else(|| AdapterError::invalid_value("'model' takes a string"))?;
                let offered = appserver::catalogue_ids(&catalogue);
                // Checked here because the vendor accepts a nonexistent model, echoes it
                // back as current, and only a `warning` notification ever mentions it.
                if !offered.is_empty() && !offered.contains(&requested) {
                    return Err(AdapterError::invalid_value(format!(
                        "codex does not offer the model '{requested}'; this install \
                         enumerated {}",
                        offered.join(", ")
                    ))
                    .into());
                }
                ("model", json!(requested))
            }
            "effort" => {
                let requested = text
                    .clone()
                    .ok_or_else(|| AdapterError::invalid_value("'effort' takes a string"))?;
                // **Per model**, because `supportedReasoningEfforts` is a field on a
                // catalogue entry rather than a session-global list.
                let ladder = model
                    .as_deref()
                    .and_then(|model| appserver::supported_efforts(&catalogue, model));
                if let Some(ladder) = ladder {
                    if !ladder.contains(&requested) {
                        return Err(AdapterError::invalid_value(format!(
                            "'{requested}' is not a reasoning effort {} supports; it \
                             enumerated {}",
                            model.as_deref().unwrap_or("this model"),
                            ladder.join(", ")
                        ))
                        .into());
                    }
                }
                ("effort", json!(requested))
            }
            "approvalPolicy" | "personality" => {
                let requested = text
                    .clone()
                    .ok_or_else(|| AdapterError::invalid_value(format!("'{id}' takes a string")))?;
                let offered: &[&str] = if id == "approvalPolicy" {
                    super::APPROVAL_POLICIES
                } else {
                    super::PERSONALITIES
                };
                // Enum-typed, so the vendor would refuse this too — checked here anyway so
                // the message names the option and its values rather than reading as a
                // JSON-RPC parse error, and so `granular` (a real variant this adapter
                // deliberately does not offer, because it is an object rather than a
                // string) is refused as unoffered rather than sent and rejected.
                if !offered.contains(&requested.as_str()) {
                    return Err(AdapterError::invalid_value(format!(
                        "'{requested}' is not a value '{id}' offers; codex enumerated {}",
                        offered.join(", ")
                    ))
                    .into());
                }
                (id, json!(requested))
            }
            "sandbox" => {
                let requested = text
                    .clone()
                    .ok_or_else(|| AdapterError::invalid_value("'sandbox' takes a string"))?;
                let policy = appserver::sandbox_policy(&requested).ok_or_else(|| {
                    AdapterError::invalid_value(format!(
                        "'{requested}' is not a sandbox mode; codex offers {}",
                        super::SANDBOX_MODES.join(", ")
                    ))
                })?;
                // **`sandboxPolicy` as an object here, where `thread/start` took `sandbox`
                // as a string.** Two spellings of one concept in one API.
                ("sandboxPolicy", policy)
            }
            // The vendor emits no `UNSUPPORTED_FIELD` signal at all, so this is the only
            // place an unknown option can be refused rather than reported as applied.
            other => return Err(appserver::refuse_unknown(other).into()),
        };

        // **A write that cannot change anything is not sent**, which removes the wait for the
        // case that hits it most.
        //
        // Placed *after* validation, deliberately: an invalid value must still be refused
        // rather than shortcut, or a client would get a success for a value the vendor would
        // have rejected. And it compares against the **vendor's** last reported surface, not
        // against a remembered request — which is what makes it safe to skip at all.
        //
        // The gain is not merely latency. Because a no-op emits no notification, sending it
        // means waiting out `SETTINGS_ECHO_WAIT` for a confirmation that is never coming, and
        // that is precisely what re-selecting the current value does.
        let already_current = self
            .state
            .lock()
            .await
            .settings
            .as_ref()
            .and_then(|settings| settings.get(key).cloned())
            .is_some_and(|current| current == wire);
        if already_current {
            return Ok(self.capabilities().await?.config);
        }

        // **Registered before the write**, because the notification can arrive before the
        // reply — the same ordering `Child::request` needs and for the same reason.
        let echo = self.settings_echo.notified();
        let outcome = request(
            &self.child,
            method::THREAD_SETTINGS_UPDATE,
            json!({"threadId": self.thread.as_str(), key: wire}),
        )
        .await;
        if let Err(error) = outcome {
            // A vendor refusal about a real option is about *this value*, so the control
            // stays usable. The vendor's own sentence is carried through rather than
            // reworded: it lists the variants it accepts.
            return Err(AdapterError::invalid_value(format!(
                "codex refused {id} = {}: {error}",
                named(&value)
            ))
            .into());
        }

        // **A timeout is not a failure.** A write that changes nothing emits no
        // notification, so the fallback is the last known surface
        // ([`appserver::SETTINGS_REPLY_IS_EMPTY`]).
        let _ = tokio::time::timeout(SETTINGS_ECHO_WAIT, echo).await;
        Ok(self.capabilities().await?.config)
    }

    fn launch_flag_for_config(&self, id: &str, value: &Value) -> Option<(String, String)> {
        let value = value.as_str()?.to_string();
        match id {
            // `sandbox` is translated to an object only on the app-server wire. The
            // launch flag is intentionally the protocol's stable string spelling.
            "model" | "effort" | "approvalPolicy" | "sandbox" => Some((id.to_string(), value)),
            // Personality is runtime-only: the CLI has no corresponding launch flag.
            _ => None,
        }
    }

    /// Answer an approval codex raised.
    async fn resolve(&self, resolution: Resolution) -> Result<()> {
        let available = self
            .state
            .lock()
            .await
            .pending
            .remove(&resolution.request_id)
            // Measured: approvals fan out to every subscriber and `serverRequest/resolved`
            // retires them, so a request answered by somebody else is genuinely gone. Said
            // plainly rather than sent, because sending a decision for a request the vendor
            // has stopped waiting on is a frame it may answer with an error nothing reads.
            .ok_or_else(|| {
                anyhow!(
                    "codex is not waiting on request {}; it was answered or the turn ended",
                    resolution.request_id
                )
            })?;

        let decision = if available
            .iter()
            .any(|offered| offered.as_str() == Some(resolution.option.as_str()))
        {
            Decision::Token(resolution.option.clone())
        } else {
            // An object variant: the payload is **echoed from the offer**, never rebuilt.
            // Reconstructing an amendment invites one that does not match what the user was
            // shown — they approved a specific execpolicy change, not the idea of one.
            let offered = available
                .iter()
                .find(|offered| {
                    offered
                        .as_object()
                        .and_then(|map| map.keys().next())
                        .map(String::as_str)
                        == Some(resolution.option.as_str())
                })
                .and_then(Value::as_object)
                .and_then(|map| map.iter().next());
            match offered {
                Some((name, payload)) => Decision::Amendment {
                    name: name.clone(),
                    payload: payload.clone(),
                },
                None => {
                    // **This is the guard the measured silent failure needed.** Sending
                    // `"approved"` — a real token from the sibling `ReviewDecision`
                    // namespace, and a plausible guess — produced no error at any layer:
                    // the turn reported `completed` and the command never ran, with the
                    // only evidence a status two levels down inside an item.
                    return Err(AdapterError::invalid_value(format!(
                        "'{}' is not a decision codex offered for request {}; it offered {}",
                        resolution.option,
                        resolution.request_id,
                        available
                            .iter()
                            .map(|offered| match offered {
                                Value::String(token) => token.clone(),
                                other => other
                                    .as_object()
                                    .and_then(|map| map.keys().next().cloned())
                                    .unwrap_or_else(|| other.to_string()),
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .into());
                }
            }
        };

        // Checked against the request's own offer even after the mapping above, because
        // that is where the authority is (`appserver::REQUEST_SCOPED_AUTHORITY`) — a token
        // in the enum but absent from this request is still refused.
        if !appserver::is_offered(&decision, &available) {
            return Err(AdapterError::invalid_value(format!(
                "codex did not offer '{}' for request {}",
                resolution.option, resolution.request_id
            ))
            .into());
        }

        // A JSON-RPC **response**, so `id` and `result` and no `method`. The id is the
        // vendor's own request id, echoed as the number it sent.
        let id: Value = resolution
            .request_id
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(resolution.request_id.clone()));
        self.child
            .send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"decision": decision.to_wire()},
            }))
            .await
    }

    fn events(&mut self) -> Option<EventStream> {
        // `try_lock` because this is the sync method on an async trait, and `&mut self`
        // means nothing else holds the session. Taking the receiver is what makes a second
        // call answer `None`.
        let receiver = self.stream.try_lock().ok()?.take()?;
        Some(Box::pin(futures::stream::unfold(
            receiver,
            |mut receiver| async move { receiver.recv().await.map(|item| (item, receiver)) },
        )))
    }

    /// Ask codex to exit, and make sure it did.
    ///
    /// Delegates to [`Child::close`], which signals the **group**: codex spawns a shell per
    /// command execution and an MCP server per configured entry, and signalling only the
    /// parent leaves a tool call running after the session is gone. Idempotent and
    /// escalating, because the idle reaper, an explicit detach, and shutdown can all reach
    /// it.
    async fn close(&self) -> Result<()> {
        self.child.close().await;
        Ok(())
    }
}

/// Send one request and await its reply, failing on a JSON-RPC error.
///
/// A thin wrapper over [`Child::request`], which owns the correlation, the timeout, and the
/// "vendor closed without answering" diagnosis. What this adds is the one thing that is
/// codex's rather than the transport's: **a JSON-RPC `error` is a refusal, so it must never
/// read as an empty success.**
///
/// That is the `claude` defect `AGENTS.md` records, one vendor over: a reader that took
/// `response.get("response") or {}` off an *error* envelope turned every refusal into the
/// same empty value a successful no-payload control returns, so a refused permission mode
/// was recorded as applied and rendered as the current one. Here the discriminant is read
/// before the payload and the vendor's own sentence is carried out.
async fn request(child: &Child, method: &str, params: Value) -> Result<Value> {
    let reply = child
        .request(|id| json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        .await
        .with_context(|| format!("codex did not answer {method}"))?;

    if let Some(error) = reply.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("codex refused the request without a message");
        return Err(sylph_protocol::VendorRefusal {
            message: message.to_string(),
        }
        .into());
    }
    Ok(reply)
}

/// Drain the vendor's stdout for the session's life.
async fn read_loop(
    mut lines: mpsc::Receiver<Line>,
    tx: mpsc::Sender<Result<SessionMessage>>,
    state: Arc<Mutex<State>>,
    settings_echo: Arc<Notify>,
    child: Arc<Child>,
    source: String,
) {
    let mut translator: Option<Translator> = None;
    let mut version = 0u64;
    let mut native = super::native::LiveRecords::new();
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(250));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let line = tokio::select! {
            line = lines.recv() => line,
            _ = poll.tick() => {
                let id = state.lock().await.thread.clone();
                if let Some(id) = id {
                    match native.poll(&NativeId::new(id)) {
                        Ok(messages) => for message in messages {
                            if tx.send(Ok(message)).await.is_err() { child.close().await; return; }
                        },
                        Err(error) => { if tx.send(Err(error)).await.is_err() { return; } }
                    }
                }
                continue;
            }
        };
        let Some(line) = line else {
            if let Some(id) = state.lock().await.thread.clone() {
                match native.poll(&NativeId::new(id)) {
                    Ok(messages) => {
                        for message in messages {
                            if tx.send(Ok(message)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error)).await;
                    }
                }
            }
            break;
        };
        let (frame, raw) = match line {
            Ok(line) => (line.value, line.raw),
            Err(error) => {
                // **Reported, not fatal.** A malformed line must not fail a turn, and an
                // `Err` on this stream is a failure *reading* the session rather than a
                // failure of the session.
                if tx.send(Err(error)).await.is_err() {
                    break;
                }
                continue;
            }
        };
        // **Built once, and never gated on the thread id being known.**
        //
        // An earlier version waited for `State::thread` before translating anything, on the
        // reasoning that a frame cannot be attributed to a thread until the open reports one.
        // That was a race, and a *lost frame* rather than a late one: this loop and `open`
        // run on different tasks, so a frame the vendor emits immediately after
        // `thread/start`'s reply — which is exactly when an approval or a first `item/*`
        // arrives — reaches here before `open` has stored the id, and was dropped with
        // nothing reporting it.
        //
        // It presented as a *flaky* test rather than a broken one, because whether the
        // vendor's next frame or the waiting task wins is timing: the collision test failed
        // roughly one run in five alone, and every run under a loaded suite. Which is the
        // shape `AGENTS.md` names — a defect whose only symptom is a race is invisible to
        // whichever side happens to win it.
        //
        // Nothing in translation actually needs the id except the delegation edge, which
        // takes it as an `Option` and says what it does when it is absent.
        let translator = translator.get_or_insert_with(|| Translator {
            state: state.clone(),
            settings_echo: settings_echo.clone(),
        });
        version = version.saturating_add(1);
        if tx
            .send(Ok(SessionMessage::NativeRecord(NativeRecord {
                source: source.clone(),
                version,
                raw_json: Some(raw),
                payload: frame.clone(),
                blobs: Vec::new(),
            })))
            .await
            .is_err()
        {
            child.close().await;
            return;
        }
        for message in translator.frame(&frame).await {
            if tx.send(message).await.is_err() {
                // The server dropped the session. Nothing is left to report to, so the
                // process is asked to exit rather than left holding a warmed context
                // nobody can reach.
                child.close().await;
                return;
            }
        }
    }
    // stdout closed: the vendor exited. Dropping `tx` ends the stream, which is what a
    // `Stream` already expresses — there is no `Closed` event and no exit code to carry.
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn next_projection(stream: &mut EventStream) -> Option<Result<SessionMessage>> {
        use futures::StreamExt;
        while let Some(message) = stream.next().await {
            if !matches!(message, Ok(SessionMessage::NativeRecord(_))) {
                return Some(message);
            }
        }
        None
    }

    /// A translator over a fresh session whose thread id is `thread`.
    async fn translator(thread: &str) -> Translator {
        let state = Arc::new(Mutex::new(State {
            thread: Some(thread.to_string()),
            ..State::default()
        }));
        Translator {
            state,
            settings_echo: Arc::new(Notify::new()),
        }
    }

    /// Every event one frame produced.
    async fn events(translator: &Translator, frame: Value) -> Vec<Event> {
        translator
            .frame(&frame)
            .await
            .into_iter()
            .filter_map(|message| match message.ok()? {
                SessionMessage::Event(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    /// One event, or a panic naming what arrived instead.
    async fn only_event(translator: &Translator, frame: Value) -> Event {
        let mut produced = events(translator, frame).await;
        assert_eq!(produced.len(), 1, "expected one event, got {produced:?}");
        produced.remove(0)
    }

    /// **A delta's stream id is on `itemId`, not `id`.**
    ///
    /// The frame is copied from `docs/measurements/codex/captures/s02_turn.jsonl`. Reading
    /// `id` yields `None`, so every delta would open a block of its own and one message
    /// would render as one row per token — and nothing would error, because a `None`
    /// `stream_id` is legal on an event.
    #[tokio::test]
    async fn a_delta_carries_the_item_id_the_vendor_spells_itemid() {
        let translator = translator("01a0460c-c359-7ed3-9e13-212c0a33d6c0").await;
        let event = only_event(
            &translator,
            json!({
                "method": "item/agentMessage/delta",
                "params": {
                    "threadId": "01a0460c-c359-7ed3-9e13-212c0a33d6c0",
                    "turnId": "01a0460c-c3be-7850-8d27-cac444167b01",
                    "itemId": "msg_08ee54e1b830ae33016a90e8af266887d1b1a01d1a3d731110",
                    "delta": "done",
                },
                "emittedAtMs": 1787881647371i64,
            }),
        )
        .await;

        assert_eq!(event.kind, EventKind::Assistant);
        assert_eq!(event.phase, Some(Phase::Delta));
        assert_eq!(
            event.stream_id.as_deref(),
            Some("msg_08ee54e1b830ae33016a90e8af266887d1b1a01d1a3d731110"),
        );
        assert_eq!(event.text, "done");
        // The vendor's own stamp, not the clock: an adapter that re-stamps loses the pacing
        // that makes this the one `stream`-fidelity path.
        assert_eq!(event.created_at_ms, 1787881647371);
        // A delta is a projection, so it must not reach the journal.
        assert!(!event.is_stable());
    }

    /// A command execution: the call at `started`, the outcome at `completed`.
    ///
    /// Both frames are verbatim from `s02_turn.jsonl`. The `completed` half asserts the
    /// output text and the exit code, because "it produced an event" would pass on a reader
    /// that dropped both.
    #[tokio::test]
    async fn a_command_execution_yields_a_call_then_a_result() {
        let translator = translator("t").await;
        let item = |status: &str, output: Value, exit: Value| {
            json!({
                "method": "item/completed",
                "params": {"item": {
                    "type": "commandExecution",
                    "id": "exec-19dc1194-088e-48f3-9c95-1b6b38d0ed35",
                    "command": "/bin/zsh -lc 'echo ok'",
                    "cwd": "/tmp/codex-probe-work",
                    "status": status,
                    "aggregatedOutput": output,
                    "exitCode": exit,
                    "durationMs": 0,
                }},
                "emittedAtMs": 1787881644652i64,
            })
        };

        let call = only_event(
            &translator,
            json!({
                "method": "item/started",
                "params": {"item": {
                    "type": "commandExecution",
                    "id": "exec-19dc1194-088e-48f3-9c95-1b6b38d0ed35",
                    "command": "/bin/zsh -lc 'echo ok'",
                    "status": "inProgress",
                    "aggregatedOutput": Value::Null,
                    "exitCode": Value::Null,
                }},
            }),
        )
        .await;
        assert_eq!(call.kind, EventKind::ToolCall);
        assert_eq!(call.name.as_deref(), Some("exec"));
        assert_eq!(call.text, "/bin/zsh -lc 'echo ok'");
        assert!(!call.is_error);

        let result = only_event(&translator, item("completed", json!("ok\n"), json!(0))).await;
        assert_eq!(result.kind, EventKind::ToolResult);
        assert_eq!(
            result.stream_id.as_deref(),
            Some("exec-19dc1194-088e-48f3-9c95-1b6b38d0ed35")
        );
        assert_eq!(result.text, "ok\n");
        assert_eq!(result.data.as_ref().unwrap()["exitCode"], json!(0));
        assert!(!result.is_error);

        // **`status` is the discriminant, not the exit code.** An `mcpToolCall` and a
        // `fileChange` carry no exit code at all, so a reader keyed on it reports every
        // failed MCP call as a success — measured in the store at 12 of 18.
        let failed = only_event(
            &translator,
            json!({
                "method": "item/completed",
                "params": {"item": {
                    "type": "mcpToolCall",
                    "id": "call_x",
                    "status": "failed",
                }},
            }),
        )
        .await;
        assert!(
            failed.is_error,
            "a failed item with no exit code is still a failure"
        );
        assert_eq!(failed.name.as_deref(), Some("mcp"));
    }

    /// **An empty `reasoning` item emits nothing.**
    ///
    /// Measured: every `reasoning` item in the captures carries `summary: []` and
    /// `content: []`, matching the store's 4895 items. Emitting one would put a blank
    /// thinking block in every transcript, which reads as a rendering bug rather than as an
    /// absent record — and plaintext, when it arrives, must still be kept.
    #[tokio::test]
    async fn reasoning_is_emitted_only_when_it_carries_plaintext() {
        let translator = translator("t").await;
        let frame = |summary: Value| {
            json!({
                "method": "item/completed",
                "params": {"item": {
                    "type": "reasoning", "id": "rs_1", "summary": summary, "content": [],
                }},
            })
        };

        assert!(events(&translator, frame(json!([]))).await.is_empty());

        let event = only_event(
            &translator,
            frame(json!([{"type": "text", "text": "weighing"}])),
        )
        .await;
        assert_eq!(event.kind, EventKind::Thinking);
        assert_eq!(event.text, "weighing");
    }

    /// **A turn's outcome is read from the vendor, and all three statuses are distinct.**
    ///
    /// This is the `pi` defect in this file's own vendor: an adapter that emitted a
    /// hardcoded `stopReason: "end_turn"` turned a model that 404'd every request into a
    /// turn that closed clean and said nothing. All three statuses are measured — `completed`
    /// in `s02_turn.jsonl`, `failed` in `s05_effort_turn.jsonl`, `interrupted` in
    /// `s03_control.jsonl`.
    #[tokio::test]
    async fn a_turns_outcome_is_the_vendors_and_a_failure_says_why() {
        let translator = translator("t").await;
        let close = |status: &str, error: Value| {
            json!({
                "method": "turn/completed",
                "params": {"turn": {
                    "id": "01a0460c-c3be-7850-8d27-cac444167b01",
                    "status": status,
                    "error": error,
                    "startedAt": 1787881636i64,
                    "completedAt": 1787881647i64,
                    "durationMs": 10703,
                }},
                "emittedAtMs": 1787881647510i64,
            })
        };

        let completed = only_event(&translator, close("completed", Value::Null)).await;
        assert_eq!(completed.kind, EventKind::Turn);
        assert_eq!(completed.phase, Some(Phase::Completed));
        assert_eq!(completed.data.as_ref().unwrap()["stopReason"], "completed");
        assert!(!completed.is_error);
        // `startedAt` is **seconds** while `emittedAtMs` beside it is milliseconds. Left
        // undivided, this event would report a turn that began in 1970.
        assert_eq!(completed.started_at_ms, Some(1787881636000));
        assert!(completed.started_at_ms.unwrap() < completed.created_at_ms);

        // The vendor's own sentence has to reach the record, or a failed turn is
        // indistinguishable from an empty one.
        let failed = only_event(
            &translator,
            close(
                "failed",
                json!({"message": "level \"banana\" not supported, valid levels: low, medium, high, xhigh, max"}),
            ),
        )
        .await;
        assert!(failed.is_error);
        assert_eq!(failed.data.as_ref().unwrap()["stopReason"], "failed");
        assert!(
            failed.text.contains("banana"),
            "the vendor's sentence: {:?}",
            failed.text
        );

        // `interrupted` is a real outcome and **not** a failure: a user's stop reported red
        // would present a deliberate stop as a broken turn.
        let interrupted = only_event(&translator, close("interrupted", Value::Null)).await;
        assert_eq!(
            interrupted.data.as_ref().unwrap()["stopReason"],
            "interrupted"
        );
        assert!(!interrupted.is_error);
    }

    /// **A turn-level `error` notification must produce a record.**
    ///
    /// Measured in `s05_effort_turn.jsonl`: the vendor reports the 400 as its own
    /// notification, *and* on the turn's close. Dropping this one would leave a session
    /// whose only evidence of a failed model is a `warning` nobody reads.
    #[tokio::test]
    async fn a_vendor_error_and_a_warning_both_reach_the_record() {
        let translator = translator("t").await;

        let error = only_event(
            &translator,
            json!({
                "method": "error",
                "params": {
                    "error": {"message": "level \"banana\" not supported", "codexErrorInfo": "other"},
                    "willRetry": false,
                    "threadId": "t",
                },
                "emittedAtMs": 1787882120542i64,
            }),
        )
        .await;
        assert_eq!(error.kind, EventKind::Notice);
        assert!(error.is_error);
        assert!(error.text.contains("banana"));
        assert_eq!(error.data.as_ref().unwrap()["willRetry"], json!(false));

        // The *only* signal that a model name was never real: `thread/settings/update`
        // accepts a bogus model and echoes it back as current.
        let warning = only_event(
            &translator,
            json!({
                "method": "warning",
                "params": {
                    "threadId": "t",
                    "message": "Model metadata for `no-such-model-xyz` not found. Defaulting to fallback metadata",
                },
            }),
        )
        .await;
        assert_eq!(warning.kind, EventKind::Notice);
        assert!(warning.text.contains("no-such-model-xyz"));
        // Advisory, not a failure: the turn may still run.
        assert!(!warning.is_error);
    }

    /// **The delegation edge is not inverted by the delegate's own record.**
    ///
    /// `agentThreadId` names the *other party*: in a parent's stream it is the child, and in
    /// a delegate's own stream a `kind: "interacted"` record names the **parent**. A reversed
    /// pointer is still a well-formed pointer, so the tree it builds is wrong rather than
    /// broken and nothing downstream reports it.
    #[tokio::test]
    async fn a_delegation_edge_is_not_inverted_by_the_delegates_own_record() {
        let item = |agent_thread: &str, path: &str| {
            json!({
                "method": "item/started",
                "params": {"item": {
                    "type": "subAgentActivity",
                    "id": "call_sjYx0OwjkvGV93cWUKDX6Lmi",
                    "kind": "started",
                    "agentThreadId": agent_thread,
                    "agentPath": path,
                }},
            })
        };
        let child_thread = "01a0462b-cdb3-75e0-b609-0a218686b187";

        // The parent's view: a delegation, named from the path's last segment, hosted by the
        // issuing call.
        let parent = translator("01a0462b-b919-7c33-9d9c-f8bec39c3cc8").await;
        let event = only_event(&parent, item(child_thread, "/root/echo_task")).await;
        assert_eq!(event.kind, EventKind::Subagent);
        assert_eq!(event.stream_id.as_deref(), Some(child_thread));
        assert_eq!(event.span.as_deref(), Some("call_sjYx0OwjkvGV93cWUKDX6Lmi"));
        assert_eq!(event.name.as_deref(), Some("echo_task"));
        assert_eq!(event.interior_state, Some(InteriorState::Partial));
        // Stable at every phase: its `started` is where the trajectory's id, name, and
        // parent live, and dropped as ephemeral the reload flattens the subtree.
        assert!(event.is_stable());

        // The delegate's own view of the same edge opens nothing, both ways it can present:
        // by naming our own thread, and by naming the primary agent's path.
        let child = translator(child_thread).await;
        assert!(
            events(&child, item(child_thread, "/root/echo_task"))
                .await
                .is_empty(),
            "our own thread as the other party is a record of the parent"
        );
        assert!(
            events(
                &child,
                item("01a0462b-b919-7c33-9d9c-f8bec39c3cc8", "/root")
            )
            .await
            .is_empty(),
            "an /root agentPath is the child's view upward"
        );
    }

    /// Child items are multiplexed onto the parent's app-server stream, so their
    /// `threadId` must become the trajectory span. Without it, the child command and
    /// answer render as root rows beside an empty delegation card.
    #[tokio::test]
    async fn child_items_are_attributed_to_their_thread_trajectory() {
        let translator = translator("parent").await;
        let event = only_event(
            &translator,
            json!({
                "method": "item/completed",
                "params": {
                    "threadId": "child",
                    "item": {
                        "type": "agentMessage",
                        "id": "msg-child",
                        "text": "child answer",
                        "phase": "final_answer"
                    }
                }
            }),
        )
        .await;
        assert_eq!(event.kind, EventKind::Assistant);
        assert_eq!(event.span.as_deref(), Some("child"));
    }

    /// A child `turn/completed` closes the delegated trajectory. It must not be emitted as a
    /// root `turn` event, because the server uses that event to settle the parent's turn.
    /// Session-scoped routing is still needed for a child boundary that arrives after the
    /// parent has already settled; this adapter test pins the first half of that contract.
    #[tokio::test]
    async fn child_turn_completion_closes_the_trajectory_not_the_root_turn() {
        let translator = translator("parent").await;
        let _ = events(
            &translator,
            json!({
                "method": "item/started",
                "params": {
                    "threadId": "parent",
                    "item": {
                        "type": "subAgentActivity",
                        "id": "call-spawn",
                        "kind": "started",
                        "agentThreadId": "child",
                        "agentPath": "/root/worker"
                    }
                }
            }),
        )
        .await;

        let child_close = events(
            &translator,
            json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "child",
                    "turn": {
                        "id": "turn-child",
                        "status": "completed",
                        "items": [{"type": "agentMessage", "text": "all done"}],
                        "startedAt": 1787883671i64,
                        "durationMs": 1000
                    }
                }
            }),
        )
        .await;

        assert_eq!(child_close.len(), 1);
        assert_eq!(child_close[0].kind, EventKind::Subagent);
        assert_eq!(child_close[0].stream_id.as_deref(), Some("child"));
        assert_eq!(child_close[0].span.as_deref(), Some("call-spawn"));
        assert_eq!(child_close[0].text, "all done");
        assert_eq!(child_close[0].interior_state, Some(InteriorState::Full));
    }

    /// Replay the measured fork capture rather than a hand-written subset. The fixture's
    /// ordering is the important part: the parent turn completes before the child's final
    /// deltas and completion, and a later parent turn starts while those frames are still
    /// arriving. A translator that routes by "currently active turn" turns the child close
    /// into a second root turn, which is the source of the `unprompted` card.
    #[tokio::test]
    async fn measured_fork_capture_keeps_child_frames_out_of_root_turns() {
        let parent = "01a0462b-b919-7c33-9d9c-f8bec39c3cc8";
        let translator = translator(parent).await;
        let capture =
            include_str!("../../../../docs/measurements/codex/captures/s10_delegation_fork.jsonl");
        let mut produced = Vec::new();
        for line in capture.lines() {
            let row: Value = serde_json::from_str(line).expect("capture row is JSON");
            if row.get("dir").and_then(Value::as_str) != Some("in") {
                continue;
            }
            let Some(frame) = row.get("frame") else {
                continue;
            };
            produced.extend(events(&translator, frame.clone()).await);
        }

        let child = "01a0462b-cdb3-75e0-b609-0a218686b187";
        assert!(
            produced
                .iter()
                .any(|event| event.span.as_deref() == Some(child)),
            "the measured child must have attributed interior events"
        );
        let child_close = produced.iter().find(|event| {
            event.kind == EventKind::Subagent
                && event.stream_id.as_deref() == Some(child)
                && event.phase == Some(Phase::Completed)
        });
        assert!(
            child_close.is_some(),
            "the child close must survive the replay"
        );
        assert!(
            produced.iter().all(|event| {
                !(event.kind == EventKind::Turn
                    && event.stream_id.as_deref() == Some("01a0462b-cde2-76d1-a20d-3613d4b20a7c"))
            }),
            "child turn/completed must never become a root turn event"
        );
    }

    /// **A ciphertext brief yields no name rather than a placeholder.**
    ///
    /// From 0.147 the `spawn_agent` brief is ciphertext, and a placeholder written as a
    /// delegation's `name` becomes its durable name — afterwards indistinguishable from one
    /// an agent chose. `None` is the only value that stays honest in a journal.
    #[tokio::test]
    async fn a_delegation_with_no_path_is_left_unnamed() {
        let translator = translator("parent").await;
        let event = only_event(
            &translator,
            json!({
                "method": "item/started",
                "params": {"item": {
                    "type": "subAgentActivity",
                    "id": "call_x",
                    "kind": "started",
                    "agentThreadId": "child",
                    "agentPath": Value::Null,
                }},
            }),
        )
        .await;
        assert!(
            event.name.is_none(),
            "an unnamed delegation must stay unnamed, got {:?}",
            event.name
        );
    }

    /// **A `collabAgentToolCall` is a barrier, not a delegation.**
    ///
    /// Its `id` names a `wait_agent` call, which is what makes treating it as lineage
    /// tempting and wrong: it would open a trajectory for the *waiting*, with no delegate to
    /// put inside it. Both real instances carry `receiverThreadIds: []`.
    #[tokio::test]
    async fn a_collaboration_barrier_opens_no_trajectory() {
        let translator = translator("01a0462b-b919-7c33-9d9c-f8bec39c3cc8").await;
        assert!(events(
            &translator,
            json!({
                "method": "item/completed",
                "params": {"item": {
                    "type": "collabAgentToolCall",
                    "id": "call_XVDJ6uygCi5YWTf8A9KnQg6B",
                    "tool": "wait",
                    "status": "completed",
                    "senderThreadId": "01a0462b-b919-7c33-9d9c-f8bec39c3cc8",
                    "receiverThreadIds": [],
                    "agentsStates": {},
                }},
            })
        )
        .await
        .is_empty());
    }

    /// A user's own message is not echoed back as an event.
    #[tokio::test]
    async fn a_user_message_is_not_echoed_as_an_event() {
        let translator = translator("t").await;
        assert!(events(
            &translator,
            json!({
                "method": "item/completed",
                "params": {"item": {
                    "type": "userMessage",
                    "id": "01a0460c-c507-7081-93aa-4287ec8c0562",
                    "content": [{"type": "text", "text": "hello"}],
                }},
            })
        )
        .await
        .is_empty());
    }

    /// The measured approval frame, verbatim from `s14_approval.jsonl`.
    fn approval_frame(id: i64) -> Value {
        json!({
            "method": "item/commandExecution/requestApproval",
            "id": id,
            "params": {
                "kind": "command",
                "threadId": "01a04631-c67a-7161-ac2d-96e78ff72dcd",
                "turnId": "01a04631-d4be-76c1-8ab0-a5836b8a76ad",
                "itemId": "exec-721068db-d78d-44e1-9f16-6849bd6aa603",
                "command": "/bin/zsh -lc 'echo needs-approval'",
                "cwd": "/tmp/codex-probe-work",
                "commandActions": [{"type": "unknown", "command": "echo needs-approval"}],
                "proposedExecpolicyAmendment": ["echo", "needs-approval"],
                "availableDecisions": [
                    "accept",
                    {"acceptWithExecpolicyAmendment": {
                        "execpolicy_amendment": ["echo", "needs-approval"]
                    }},
                    "cancel",
                ],
            },
        })
    }

    /// **An approval offers exactly what the vendor offered, including the object variant.**
    ///
    /// Every measured approval offers `accept`, an execpolicy amendment, and `cancel` — so
    /// `acceptForSession` was never on offer while the amendment always was. A global
    /// allowlist fails in both directions at once.
    #[tokio::test]
    async fn an_approval_offers_the_requests_own_decisions() {
        let translator = translator("01a04631-c67a-7161-ac2d-96e78ff72dcd").await;
        let messages = translator.frame(&approval_frame(0)).await;
        let request = match messages.into_iter().next().unwrap().unwrap() {
            SessionMessage::Request(request) => request,
            other => panic!("an approval must be a Request, got {other:?}"),
        };

        // The vendor numbers its first server request `0`, and a `Resolution` echoes the
        // decimal spelling.
        assert_eq!(request.id, "0");
        assert_eq!(request.kind, RequestKind::Approval);
        assert_eq!(
            request.tool_use_id.as_deref(),
            Some("exec-721068db-d78d-44e1-9f16-6849bd6aa603"),
            "the request must pair to the tool_call already on the timeline"
        );
        let offered: Vec<&str> = request.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(
            offered,
            ["accept", "acceptWithExecpolicyAmendment", "cancel"]
        );
        // The object variant is offered under its **member name**, which is what `resolve`
        // maps back to the payload the vendor sent.
        assert!(
            request.options[1].persistent,
            "an amendment is a lasting grant"
        );
        assert!(!request.options[0].persistent, "`accept` is once");
        assert!(request.allow_persistent);
        // An approval declares no form: its subject *is* one proposed tool input, which the
        // client renders from `detail`.
        assert!(request.form.is_none());
        assert_eq!(
            request.detail.as_ref().unwrap()["command"],
            "/bin/zsh -lc 'echo needs-approval'"
        );

        // And it is remembered, keyed by the id a resolution echoes, with the offer verbatim.
        let pending = translator.state.lock().await.pending.clone();
        assert_eq!(pending.get("0").map(Vec::len), Some(3));
    }

    /// **`serverRequest/resolved` forgets the row**, because approvals fan out to every
    /// subscriber and any of them may answer.
    ///
    /// Without this, a late `resolve` would send a decision for a request the vendor has
    /// stopped waiting on.
    #[tokio::test]
    async fn a_request_another_client_answered_is_forgotten() {
        let translator = translator("01a04631-c67a-7161-ac2d-96e78ff72dcd").await;
        translator.frame(&approval_frame(0)).await;
        assert!(translator.state.lock().await.pending.contains_key("0"));

        translator
            .frame(&json!({
                "method": "serverRequest/resolved",
                "params": {"threadId": "01a04631-c67a-7161-ac2d-96e78ff72dcd", "requestId": 0},
            }))
            .await;
        assert!(
            !translator.state.lock().await.pending.contains_key("0"),
            "a resolved request must be forgotten, or a late answer is sent to a vendor \
             that stopped waiting"
        );
    }

    /// The config surface comes from the vendor's reported settings, and the sandbox value
    /// is translated back into the spelling a client sends.
    ///
    /// The settings object is verbatim from `s04_settings.jsonl`. **Untranslated, the
    /// sandbox row can never show a checkmark**: the vendor reports `workspaceWrite` and
    /// every offered value is `workspace-write`.
    #[test]
    fn the_config_surface_reads_current_values_from_the_vendors_report() {
        let settings = json!({
            "cwd": "/tmp/codex-probe-work",
            "approvalPolicy": "on-request",
            "sandboxPolicy": {
                "type": "workspaceWrite",
                "writableRoots": [],
                "networkAccess": false,
            },
            "model": "gpt-5.6-sol",
            "effort": "low",
            "personality": "pragmatic",
        });
        let catalogue = vec![json!({
            "id": "gpt-5.6-sol",
            "displayName": "GPT-5.6-Sol",
            "description": "Latest frontier agentic coding model.",
            "supportedReasoningEfforts": [
                {"reasoningEffort": "low"}, {"reasoningEffort": "high"},
            ],
        })];

        let options = config_options(&settings, &catalogue);
        let by_id = |id: &str| {
            options
                .iter()
                .find(|option| option.id == id)
                .unwrap_or_else(|| panic!("no {id} option in {options:?}"))
        };

        assert_eq!(by_id("model").current_value, Some(json!("gpt-5.6-sol")));
        assert_eq!(by_id("model").options[0].name, "GPT-5.6-Sol");
        assert_eq!(by_id("effort").current_value, Some(json!("low")));
        // **Per model.** The ladder is this model's `supportedReasoningEfforts`, not a
        // session-global list, so a two-effort model offers two rows.
        let ladder: Vec<&str> = by_id("effort")
            .options
            .iter()
            .map(|c| c.value.as_str())
            .collect();
        assert_eq!(ladder, ["low", "high"]);
        assert_eq!(
            by_id("approvalPolicy").current_value,
            Some(json!("on-request"))
        );
        assert_eq!(by_id("personality").current_value, Some(json!("pragmatic")));

        // The translation this test exists for.
        let sandbox = by_id("sandbox");
        assert_eq!(sandbox.current_value, Some(json!("workspace-write")));
        assert!(
            sandbox
                .options
                .iter()
                .any(|choice| Some(&json!(choice.value.clone())) == sandbox.current_value.as_ref()),
            "the current sandbox value must match an offered row, or no checkmark can land"
        );

        // A value the vendor did not report is **absent**, never defaulted: `false` or a
        // guessed model would be a claim about the session.
        let unreported = config_options(&json!({}), &catalogue);
        for option in &unreported {
            assert!(
                option.current_value.is_none(),
                "{} must be absent when unreported",
                option.id
            );
        }
        // With no model reported the ladder falls back to the spawn list rather than empty,
        // so the control stays usable.
        assert!(!unreported
            .iter()
            .find(|option| option.id == "effort")
            .unwrap()
            .options
            .is_empty());
    }

    /// A settings notification is a **facet-scoped** partial, carrying `config` only.
    ///
    /// An unmentioned facet cannot be cleared — the rule `claude`'s `system/init` broke by
    /// answering a whole-surface payload carrying only the permission mode, erasing 46 slash
    /// commands on the first exchange of every session.
    #[tokio::test]
    async fn a_settings_notification_is_a_config_only_partial() {
        let translator = translator("t").await;
        let messages = translator
            .frame(&json!({
                "method": "thread/settings/updated",
                "params": {"threadId": "t", "threadSettings": {
                    "model": "gpt-5.6-sol", "effort": "high",
                }},
            }))
            .await;
        let facets = match messages.into_iter().next().unwrap().unwrap() {
            SessionMessage::Capabilities(facets) => facets,
            other => panic!("expected a capability partial, got {other:?}"),
        };
        assert!(facets.config.is_some());
        assert!(
            facets.commands.is_none(),
            "commands must not be cleared by a settings echo"
        );
        assert!(facets.subagents.is_none());
        assert!(facets.permission_mode.is_none());

        // And it is folded into the surface, so `capabilities` answers from the vendor.
        assert_eq!(
            translator.state.lock().await.settings.as_ref().unwrap()["effort"],
            "high"
        );
    }

    /// **An approval whose id collides with an outstanding request still reaches the
    /// stream.**
    ///
    /// The transport half of this is `process.rs`'s own
    /// `correlating_on_the_id_alone_swallows_a_vendor_request`. What is asserted here is the
    /// consequence at *this* layer, because it is this vendor that makes the collision
    /// reachable: codex numbers its server requests 0,1,2 in the same space
    /// `next_request_id` mints from, so the ids genuinely overlap in an ordinary session.
    ///
    /// The peer answers a `thread/settings/update` **and** raises an approval carrying the
    /// same id, in that order. Both must arrive: the caller gets its reply, and the approval
    /// reaches the event stream as a `Request`. Swapping `reply_id` for a predicate that
    /// reads any `id` puts the approval into the waiter instead, and the turn then blocks
    /// forever on a human nobody was asked.
    #[tokio::test]
    async fn an_approval_sharing_an_id_with_a_reply_still_reaches_the_stream() {
        // **The frame is written to a file and the peer `cat`s it, so no escaping is
        // involved at all.**
        //
        // A real approval's `command` is `/bin/zsh -lc 'echo needs-approval'` — quotes and
        // all — and getting that through a double-quoted `printf` nested in a single-quoted
        // `format!` script took three attempts, each failing as a parse error that read as a
        // product bug rather than as a quoting one. A file has no layers to survive, and it
        // lets the test use the *measured* payload rather than a sanitized one.
        let frame =
            std::env::temp_dir().join(format!("sylph-codex-approval-{}", std::process::id()));
        std::fs::write(&frame, serde_json::to_string(&approval_frame(1)).unwrap()).unwrap();

        // Our first request is id 1, so the peer raises its approval as id 1 too — the
        // collision this test exists for.
        // **Emitted *after* the open reply, and after a delay**, so the thread id is
        // provably stored by the time this frame is read. That decoupling is deliberate:
        // this test is about the *id space* colliding, and the sibling test below is about
        // the *timing* window. One test was covering both properties and silently stopped
        // covering the second one when the payload moved to a file.
        let script = SCRIPTED_APP_SERVER.replace(
            "# AFTER_OPEN_REPLY",
            &format!("sleep 0.2; cat {}; printf '\\n'", frame.display()),
        );

        let mut session = open_against(&script).await.expect("the peer opens");
        let mut stream = session.events().expect("the stream is yielded once");

        // The approval is on the event stream, not in a caller's reply.
        let message = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            next_projection(&mut stream),
        )
        .await
        .expect("the approval must arrive rather than being swallowed by a waiter")
        .expect("the stream is open")
        .expect("the approval is not an error");
        match message {
            SessionMessage::Request(request) => assert_eq!(request.id, "1"),
            other => panic!("expected the approval as a Request, got {other:?}"),
        }
        let _ = std::fs::remove_file(&frame);
    }

    /// **A frame arriving before the open reply is published, not dropped.**
    ///
    /// The peer emits the approval **first**, then sleeps, and only then answers
    /// `thread/start`. So the read loop provably sees it while `State::thread` is still
    /// `None` — the window is opened on purpose rather than raced into, which is what makes
    /// this deterministic where its sibling was luck.
    ///
    /// That distinction is the whole reason this test exists. `read_loop` used to build its
    /// `Translator` only once the thread id was known, and the frame was dropped with nothing
    /// reporting it. The collision test caught that about one run in five, then stopped
    /// catching it at all once the payload moved to a file — a test that catches a race by
    /// luck stops catching it the moment anything nearby changes, and nothing reports that it
    /// stopped. `AGENTS.md` states the rule one layer up: testing "it streams" needs a runner
    /// slower than one render, so testing "a frame arrives before the id is stored" needs a
    /// peer that emits into that window on purpose.
    ///
    /// **Asserts the frame is published, not merely late.** Ordering alone would pass against
    /// a version that deferred it forever, which is the same failure the defect had.
    #[tokio::test]
    async fn a_frame_arriving_before_the_open_reply_is_published() {
        let frame = std::env::temp_dir().join(format!("sylph-codex-early-{}", std::process::id()));
        std::fs::write(&frame, serde_json::to_string(&approval_frame(0)).unwrap()).unwrap();

        // The order is the test: frame, then delay, then the reply `open` is waiting on.
        let script = SCRIPTED_APP_SERVER.replace(
            "# BEFORE_OPEN_REPLY",
            &format!("cat {}; printf '\\n'; sleep 0.3", frame.display()),
        );

        let mut session = open_against(&script)
            .await
            .expect("the open still succeeds; the reply is late, not absent");
        let mut stream = session.events().expect("the stream is yielded once");

        let message = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            next_projection(&mut stream),
        )
        .await
        .expect(
            "a frame emitted before the open reply must still be published — `read_loop` \
             gating its translator on `State::thread` drops it silently, and the session \
             then waits forever on an ask no client ever saw",
        )
        .expect("the stream is open")
        .expect("the frame is not an error");

        match message {
            SessionMessage::Request(request) => {
                assert_eq!(request.id, "0");
                // Published *whole*, not as a husk: the approval is unanswerable without the
                // decisions the vendor offered, so a frame that arrived stripped would be as
                // useless as one that never arrived.
                let offered: Vec<&str> = request.options.iter().map(|o| o.id.as_str()).collect();
                assert_eq!(
                    offered,
                    ["accept", "acceptWithExecpolicyAmendment", "cancel"]
                );
            }
            other => panic!("expected the early approval as a Request, got {other:?}"),
        }

        // And the session is usable afterwards, so recovering the frame did not cost the
        // open: the thread id landed even though a frame overtook it.
        assert_eq!(
            session.native_id().as_ref().unwrap().as_str(),
            "01a0460c-c359-7ed3-9e13-212c0a33d6c0"
        );
        let _ = std::fs::remove_file(&frame);
    }

    #[tokio::test]
    async fn live_stdout_retains_lexical_raw_before_projection() {
        let raw = r#"{ "method" : "error", "params" : { "message" : "raw\\u0020warning", "future" : [1,2] } }"#;
        let script =
            SCRIPTED_APP_SERVER.replace("# AFTER_OPEN_REPLY", &format!("printf '%s\\n' '{}'", raw));
        let mut session = open_against(&script).await.expect("the peer opens");
        let mut stream = session.events().expect("the stream is yielded once");
        let raw_message = tokio::time::timeout(std::time::Duration::from_secs(2), async { loop {
            let item = futures::StreamExt::next(&mut stream)
                .await
                .unwrap()
                .unwrap();
            if matches!(&item, SessionMessage::NativeRecord(record) if record.payload["params"].get("future").is_some()) {
                break item;
            }
        }}).await.expect("the scripted raw frame arrives");
        let SessionMessage::NativeRecord(record) = raw_message else {
            unreachable!()
        };
        assert_eq!(record.raw_json.as_deref(), Some(raw.as_bytes()));
        assert_eq!(record.payload["params"]["future"], json!([1, 2]));
        assert!(record.version >= 1);
        assert!(record.source.starts_with("live/stdout/"));
        let projection = futures::StreamExt::next(&mut stream)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(projection, SessionMessage::Event(event) if event.kind == EventKind::Notice)
        );
    }

    /// A full open over a scripted peer: handshake, catalogue, thread, and the id the
    /// **vendor** chose.
    ///
    /// The peer answers the three requests in order and then holds stdout open, as the real
    /// app-server does.
    #[tokio::test]
    async fn an_open_reports_the_thread_id_the_vendor_assigned() {
        let session = open_against(SCRIPTED_APP_SERVER)
            .await
            .expect("the peer opens");
        // Reported by the vendor, not minted here. `dictates_native_id` is false for this
        // agent, so this value is the only thing that makes the session resumable.
        assert_eq!(
            session.native_id().as_ref().unwrap().as_str(),
            "01a0460c-c359-7ed3-9e13-212c0a33d6c0"
        );

        // The opening surface comes from the open reply, whose member names differ from the
        // notification's — `reasoningEffort` there, `effort` here.
        let surface = session.capabilities().await.unwrap();
        let effort = surface.config_option("effort").expect("an effort option");
        assert_eq!(effort.current_value, Some(json!("high")));
        let model = surface.config_option("model").expect("a model option");
        assert_eq!(model.current_value, Some(json!("gpt-5.6-sol")));
        // codex has no command concept at all, and a client must not read that as failure.
        assert!(surface.commands.is_empty());
    }

    /// **An open reporting no id is a failure, not a degraded success.**
    ///
    /// Without an id the session can never be resumed, so every link death would make it
    /// permanently unlinkable. Both shapes are refused: a missing member and an empty
    /// string, since `NativeId::new` is deliberately infallible and would accept `""`.
    #[tokio::test]
    async fn an_open_without_a_thread_id_fails() {
        for thread in [r#"{}"#, r#"{"id":""}"#, r#"{"id":"   "}"#] {
            let script = format!(
                r#"while read -r line; do
                     id=$(printf '%s' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
                     case "$line" in
                       *thread/start*) printf '{{"id":%s,"result":{{"thread":{}}}}}\n' "$id" '{thread}' ;;
                       *model/list*)   printf '{{"id":%s,"result":{{"data":[]}}}}\n' "$id" ;;
                       *initialize*)   printf '{{"id":%s,"result":{{}}}}\n' "$id" ;;
                     esac
                   done"#,
                "%s",
            );
            let error = match open_against(&script).await {
                // `dyn Session` is not `Debug`, so the failure is unwrapped by hand rather
                // than with `expect_err`.
                Ok(session) => panic!(
                    "an open with no reported id must fail, but it answered with {}",
                    session.native_id().as_ref().unwrap()
                ),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("without reporting its id"),
                "the error must name the missing id: {error}"
            );
        }
    }

    /// A peer that answers the open sequence and then holds stdout open.
    ///
    /// Faithful in the two ways that matter: it echoes each request's id back as a JSON
    /// **number**, and it interleaves `thread/started` with the reply — which is what
    /// requires the read loop to be draining before the open is awaited.
    ///
    /// The id is extracted non-greedily, because a greedy `.*"id":\([0-9]*\)` would match a
    /// number inside `params` instead of the request's own id.
    const SCRIPTED_APP_SERVER: &str = r#"
      while read -r line; do
        # Anchored on the member and stopping at the comma, so it neither depends on
        # serde_json's key order nor matches a number inside `params`.
        id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\),.*/\1/p')
        # **A notification carries no id, and must not be answered.** `initialized` is one,
        # and answering it emitted `{"id":,...}` — a malformed line that surfaced as an
        # unparsable-vendor-line error on the event stream, two layers from this arm.
        if [ -z "$id" ]; then continue; fi
        case "$line" in
          *'"method":"initialize"'*)
            printf '{"id":%s,"result":{"codexHome":"/tmp","platformOs":"macos"}}\n' "$id"
            printf '{"method":"remoteControl/status/changed","params":{"status":"disabled"}}\n'
            ;;
          *'"method":"model/list"'*)
            printf '{"id":%s,"result":{"data":[{"id":"gpt-5.6-sol","displayName":"GPT-5.6-Sol","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}]}}\n' "$id"
            ;;
          *'"method":"thread/start"'*|*'"method":"thread/resume"'*)
            # BEFORE_OPEN_REPLY — emits into the window where `State::thread` is still
            # `None`, on purpose. See `a_frame_arriving_before_the_open_reply_is_published`.
            printf '{"id":%s,"result":{"thread":{"id":"01a0460c-c359-7ed3-9e13-212c0a33d6c0"},"model":"gpt-5.6-sol","reasoningEffort":"high","approvalPolicy":"never","sandbox":{"type":"workspaceWrite"}}}\n' "$id"
            printf '{"method":"thread/started","params":{"thread":{"id":"01a0460c-c359-7ed3-9e13-212c0a33d6c0"}}}\n'
            # AFTER_OPEN_REPLY
            ;;
          *'"method":"turn/start"'*)
            printf '{"id":%s,"result":{"turn":{"id":"01a0460c-c3be-7850-8d27-cac444167b01","status":"inProgress"}}}\n' "$id"
            ;;
          *'"method":"thread/settings/update"'*)
            # RECORD_SETTINGS_WRITE
            printf '{"id":%s,"result":{}}\n' "$id"
            printf '{"method":"thread/settings/updated","params":{"threadId":"01a0460c-c359-7ed3-9e13-212c0a33d6c0","threadSettings":{"model":"gpt-5.6-sol","effort":"low","approvalPolicy":"never","sandboxPolicy":{"type":"workspaceWrite"}}}}\n'
            ;;
          *'"method":"turn/interrupt"'*|*'"method":"turn/steer"'*)
            # RECORD_CONTROL
            printf '{"id":%s,"result":{}}\n' "$id"
            ;;
          *)
            printf '{"id":%s,"result":{}}\n' "$id"
            ;;
        esac
      done
    "#;

    /// **The two refusal codes are not interchangeable**, and only one of them may retire a
    /// control.
    ///
    /// `UnsupportedField` means "no such option" and justifies disabling the picker;
    /// `InvalidValue` means "this value, right now" and must leave it usable. This vendor
    /// emits **no** unsupported-field signal at all — an unknown option name is accepted
    /// silently with `result: {}` — so both codes are produced here or the distinction is
    /// lost.
    #[tokio::test]
    async fn set_config_refuses_with_the_code_that_says_what_a_client_may_do() {
        let session = open_against(SCRIPTED_APP_SERVER).await.unwrap();

        let code = |error: &anyhow::Error| {
            error
                .downcast_ref::<AdapterError>()
                .expect("a refusal must travel as a typed AdapterError")
                .code
        };

        // No such option. The vendor would accept this silently and change nothing, so the
        // refusal exists only because the adapter makes it.
        let error = session
            .set_config("noSuchOption", json!("x"))
            .await
            .expect_err("an unknown option must be refused");
        assert_eq!(code(&error), sylph_protocol::RefusalCode::UnsupportedField);

        // A bogus effort. Measured: the vendor accepts `"banana"`, echoes it back as the
        // current value **and** as `collaborationMode.settings.reasoning_effort`, and fails
        // only when a turn runs. Validated here against this model's own ladder.
        let error = session
            .set_config("effort", json!("banana"))
            .await
            .expect_err("an effort outside the model's ladder must be refused");
        assert_eq!(code(&error), sylph_protocol::RefusalCode::InvalidValue);
        assert!(
            error.to_string().contains("low, high"),
            "the message must name what the vendor enumerated: {error}"
        );

        // A nonexistent model, which the vendor also accepts and echoes back.
        let error = session
            .set_config("model", json!("no-such-model-xyz"))
            .await
            .expect_err("a model outside the catalogue must be refused");
        assert_eq!(code(&error), sylph_protocol::RefusalCode::InvalidValue);

        // `granular` is a real `AskForApproval` variant this adapter deliberately does not
        // offer, because the schema types it as an object rather than a string. Refused as
        // unoffered rather than sent and rejected.
        let error = session
            .set_config("approvalPolicy", json!("granular"))
            .await
            .expect_err("a value that is not a string token must be refused");
        assert_eq!(code(&error), sylph_protocol::RefusalCode::InvalidValue);

        // And an accepted write answers with the **refreshed array**, from the vendor's
        // notification rather than from the request.
        let refreshed = session
            .set_config("effort", json!("low"))
            .await
            .expect("an offered effort is accepted");
        let effort = refreshed
            .iter()
            .find(|option| option.id == "effort")
            .expect("the refreshed array carries effort");
        assert_eq!(effort.current_value, Some(json!("low")));
    }

    /// **A write that cannot change anything is never sent.**
    ///
    /// Observed by counting what the peer received, not by timing: the peer appends every
    /// `thread/settings/update` to a file, and after two identical `set_config` calls that
    /// file holds exactly one. Timing could not distinguish these, because the scripted peer
    /// notifies on every write while a real no-op notifies on none — which is the whole
    /// reason the skip matters (`appserver::SETTINGS_REPLY_IS_EMPTY`).
    #[tokio::test]
    async fn a_write_that_cannot_change_anything_is_not_sent() {
        let log = std::env::temp_dir().join(format!("sylph-codex-writes-{}", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let script = SCRIPTED_APP_SERVER.replace(
            "# RECORD_SETTINGS_WRITE",
            &format!("printf 'write\\n' >> {}", log.display()),
        );

        let session = open_against(&script).await.expect("the peer opens");
        // The open reports `reasoningEffort: "high"`, so this one is a real change.
        session
            .set_config("effort", json!("low"))
            .await
            .expect("a real change is written");
        let after_first = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(
            after_first.lines().count(),
            1,
            "the first write reaches the vendor"
        );

        // The peer's notification reported `effort: "low"`, so this is now current.
        let refreshed = session
            .set_config("effort", json!("low"))
            .await
            .expect("a no-op still answers with the surface");
        let after_second = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(
            after_second.lines().count(),
            1,
            "a value already current must not be written again"
        );

        // And it still answers the refreshed array rather than an empty one, so a client
        // re-rendering from the result sees the checkmark where it belongs.
        let effort = refreshed
            .iter()
            .find(|option| option.id == "effort")
            .expect("the answer carries effort");
        assert_eq!(effort.current_value, Some(json!("low")));

        let _ = std::fs::remove_file(&log);
    }

    /// A turn is accepted, and the turn id needed to steer it comes from the **reply**.
    ///
    /// `turn/started` arrives 13 ms after the reply — measured — so an adapter that waited
    /// for the notification would make a fast client's steer fail its own
    /// `expectedTurnId` precondition.
    #[tokio::test]
    async fn a_turn_is_accepted_and_its_id_is_available_to_steer_immediately() {
        let session = open_against(SCRIPTED_APP_SERVER).await.unwrap();
        session
            .send(TurnInput {
                text: "reply with ok".into(),
                ..TurnInput::default()
            })
            .await
            .expect("the turn is accepted");

        // Both controls need the turn id, and both would fail with "nothing is running" if
        // the id had not been read from the reply.
        session
            .steer("and stop", None)
            .await
            .expect("a steer reaches the vendor");
        session
            .interrupt()
            .await
            .expect("an interrupt reaches the vendor");
    }

    #[tokio::test]
    async fn an_interrupt_keeps_the_accepted_turn_identity_when_live_state_advances() {
        let log =
            std::env::temp_dir().join(format!("sylph-codex-interrupt-{}", uuid::Uuid::new_v4()));
        let script = SCRIPTED_APP_SERVER.replace(
            "# RECORD_CONTROL",
            &format!("printf '%s\\n' \"$line\" >> '{}'", log.display()),
        );
        let session = open_against(&script).await.unwrap();
        session
            .send(TurnInput {
                text: "start".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let accepted = session.last_sent_turn_id().await.unwrap();
        // The vendor reader can already know a successor while the envagent still
        // holds the previous lease. Make that window explicit before dispatch.
        session.state.lock().await.turn = Some("later-native-turn".into());
        session.interrupt().await.unwrap();
        let command: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
        assert_eq!(command["params"]["turnId"], accepted);
        std::fs::remove_file(log).unwrap();
    }

    /// **A turn with neither text nor an attachment is refused rather than sent empty.**
    #[tokio::test]
    async fn an_empty_turn_is_refused() {
        let session = open_against(SCRIPTED_APP_SERVER).await.unwrap();
        assert!(session.send(TurnInput::default()).await.is_err());
    }

    /// Interrupt and steer say "nothing is running" rather than refusing the capability.
    ///
    /// The distinction matters: `Capability::Interrupt` *is* offered by this agent, so a
    /// client that disabled its stop button over this refusal would lose it for the
    /// session's life.
    #[tokio::test]
    async fn interrupting_with_no_turn_running_is_not_a_capability_refusal() {
        let session = open_against(SCRIPTED_APP_SERVER).await.unwrap();
        let error = session.interrupt().await.expect_err("no turn is running");
        assert!(error.to_string().contains("no codex turn is running"));
        assert!(
            error.downcast_ref::<AdapterError>().is_none(),
            "this is not a refusal about the control, so it must not carry a refusal code"
        );
        assert!(session.steer("hello", None).await.is_err());
    }

    /// **The decision the vendor offered is echoed back; anything else is refused.**
    ///
    /// This is the hazard that cost the most to find: sending `"approved"` — a real token
    /// from the sibling `ReviewDecision` namespace — produced **no error at any layer**. The
    /// JSON-RPC reply was accepted, the turn reported `completed`, and the command silently
    /// did not run. Measured three ways: `accept` ran the command in 72 ms with exit code 0,
    /// `decline` reported `declined`, and the bogus token reported `failed` in 3 ms.
    #[tokio::test]
    async fn an_approval_accepts_only_a_decision_the_vendor_offered() {
        let session = open_against(SCRIPTED_APP_SERVER).await.unwrap();
        // Raise the approval through the translator, as the read loop would.
        let translator = Translator {
            state: session.state.clone(),
            settings_echo: Arc::new(Notify::new()),
        };
        translator.frame(&approval_frame(0)).await;

        // The crossed-namespace token, which is what shipped as a silent total failure.
        let error = session
            .resolve(Resolution {
                request_id: "0".into(),
                option: "approved".into(),
                input: None,
            })
            .await
            .expect_err("a ReviewDecision token must be refused for a command approval");
        assert!(
            error.to_string().contains("not a decision codex offered"),
            "the refusal must say the decision was not offered: {error}"
        );
        // And it must list what *was* offered, so the client is not left guessing.
        assert!(error.to_string().contains("acceptWithExecpolicyAmendment"));

        // `acceptForSession` is in the enum and was **not offered here**, which is the other
        // direction a global allowlist failed in.
        translator.frame(&approval_frame(0)).await;
        assert!(session
            .resolve(Resolution {
                request_id: "0".into(),
                option: "acceptForSession".into(),
                input: None,
            })
            .await
            .is_err());

        // The offered object variant is accepted, and the row is consumed.
        translator.frame(&approval_frame(0)).await;
        session
            .resolve(Resolution {
                request_id: "0".into(),
                option: "acceptWithExecpolicyAmendment".into(),
                input: None,
            })
            .await
            .expect("an offered amendment is answerable");

        // A second answer to the same request is refused rather than sent: the vendor has
        // stopped waiting, and approvals fan out so somebody else may have answered.
        assert!(session
            .resolve(Resolution {
                request_id: "0".into(),
                option: "accept".into(),
                input: None,
            })
            .await
            .is_err());
    }

    fn test_spec() -> RunSpec {
        RunSpec {
            workdir: "/tmp".into(),
            native_id: sylph_protocol::RequestedNativeId::New,
            inject_identity: false,
            identity_preamble: None,
            flags: std::collections::BTreeMap::new(),
            permission_mode: None,
        }
    }

    async fn open_against(script: &str) -> Result<CodexSession> {
        let (child, lines) = Child::spawn(scripted_peer(script)).await.unwrap();
        open(child, lines, test_spec()).await
    }
}
