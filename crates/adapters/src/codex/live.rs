//! Codex process controls and capability discovery. Exact process bytes are retained
//! before routing; the server-owned pure mapper interprets all trace semantics.

use std::collections::HashMap;
use std::sync::Arc;
use sylph_protocol::{trace::ObservationRef, ControlNotice, RawCaptureSink};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use sylph_protocol::{
    AdapterError, CapabilityFacets, ConfigChoice, ConfigKind, ConfigOption, ControlStream,
    NativeId, Request, RequestKind, RequestOption, Resolution, RunSpec, Session,
    SessionCapabilities, SessionMessage, TurnInput,
};
use tokio::sync::{mpsc, Mutex, Notify};

use super::appserver::{self, method, notification, Decision};
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

/// Operational state retained between vendor control frames.
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
    /// Held in shared state because because the read loop is spawned *before* the
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
}

/// A live codex session.
pub struct CodexSession {
    thread: NativeId,
    child: Arc<Child>,
    state: Arc<Mutex<State>>,
    /// Yielded once by [`Session::messages`], because two consumers of one process's stdout
    /// would each see half the turn.
    stream: Mutex<Option<mpsc::Receiver<Result<SessionMessage>>>>,
    /// Raised whenever a [`notification::SETTINGS_UPDATED`] has been folded into
    /// [`State::settings`], so `set_config` can answer from the vendor's confirmation.
    settings_echo: Arc<Notify>,
    identity_evidence: Vec<ObservationRef>,
}

struct ControlRouter {
    state: Arc<Mutex<State>>,
    settings_echo: Arc<Notify>,
}
impl ControlRouter {
    async fn frame(&self, frame: &Value) -> Vec<Result<SessionMessage>> {
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            return vec![];
        };
        let params = frame.get("params").unwrap_or(&Value::Null);
        match method {
            notification::SETTINGS_UPDATED => self.settings(params).await,
            notification::COMMAND_APPROVAL => self.approval(frame, params).await,
            notification::SERVER_REQUEST_RESOLVED => self.resolved(params).await,
            notification::TURN_STARTED | notification::TURN_COMPLETED => {
                let mut state = self.state.lock().await;
                let Some(thread) = params.get("threadId").and_then(Value::as_str) else {
                    return vec![];
                };
                if state.thread.as_deref() != Some(thread) {
                    return vec![];
                }
                let turn = params
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let active = method == notification::TURN_STARTED;
                // A late completion cannot clear the successor observed on the same thread.
                if turn.is_none() || (!active && state.turn != turn) {
                    return vec![];
                }
                state.turn = if active { turn.clone() } else { None };
                let notice = if active {
                    ControlNotice::Active {
                        native_id: NativeId::new(thread),
                        native_turn_id: turn,
                    }
                } else {
                    ControlNotice::Settled {
                        native_id: NativeId::new(thread),
                        native_turn_id: turn,
                    }
                };
                vec![Ok(SessionMessage::Control(notice))]
            }
            _ => vec![],
        }
    }
    async fn settings(&self, params: &Value) -> Vec<Result<SessionMessage>> {
        if params.get("threadId").and_then(Value::as_str)
            != self.state.lock().await.thread.as_deref()
        {
            return vec![];
        }
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
    async fn approval(&self, frame: &Value, params: &Value) -> Vec<Result<SessionMessage>> {
        if params.get("threadId").and_then(Value::as_str)
            != self.state.lock().await.thread.as_deref()
        {
            return vec![];
        }
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
pub async fn run(spec: RunSpec, capture: Arc<dyn RawCaptureSink>) -> Result<Box<dyn Session>> {
    // Decided before the process starts, so a dictated id is refused without having spawned
    // anything.
    appserver::thread_start_params(&spec)?;

    let (child, lines) = Child::spawn(
        process::Spawn {
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
        },
        capture,
    )
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

    let opened = request_receipt(&child, open_method, open_params)
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
        .ok_or_else(|| anyhow!("codex answered {open_method} without a result: {opened:?}"))?;
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
        identity_evidence: vec![opened.observation.clone()],
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
    async fn identity_evidence(&self) -> Vec<ObservationRef> {
        self.identity_evidence.clone()
    }

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

    fn messages(&mut self) -> Option<ControlStream> {
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
    Ok(request_receipt(child, method, params).await?.value)
}
async fn request_receipt(
    child: &Child,
    method: &str,
    params: Value,
) -> Result<crate::process::ProcessLine> {
    let reply = child
        .request_with_receipt(|id| json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        .await
        .with_context(|| format!("codex did not answer {method}"))?;
    if let Some(error) = reply.get("error") {
        return Err(sylph_protocol::VendorRefusal {
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Codex refused request")
                .into(),
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
) {
    let router = ControlRouter {
        state,
        settings_echo,
    };
    while let Some(line) = lines.recv().await {
        let frame = match line {
            Ok(line) => line,
            Err(error) => {
                if tx.send(Err(error)).await.is_err() {
                    break;
                }
                continue;
            }
        };
        for message in router.frame(&frame.value).await {
            if tx.send(message).await.is_err() {
                child.close().await;
                return;
            }
        }
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;
    use sylph_protocol::RequestedNativeId;

    fn spec() -> RunSpec {
        RunSpec {
            workdir: "/tmp".into(),
            native_id: RequestedNativeId::New,
            inject_identity: false,
            identity_preamble: None,
            flags: Default::default(),
            permission_mode: None,
        }
    }

    #[tokio::test]
    async fn opening_identity_cites_the_retained_correlated_vendor_reply() {
        let script = r#"
read initialize
printf '%s\n' '{"id":1,"result":{}}'
read initialized
read models
printf '%s\n' '{"id":2,"result":{"data":[]}}'
read open
printf '%s\n' '{"id":3,"result":{"thread":{"id":"vendor-thread"},"model":"reported-model","reasoningEffort":"low"}}'
cat >/dev/null
"#;
        let capture = Arc::new(process::RecordingCapture::default());
        let (child, lines) = Child::spawn(scripted_peer(script), capture.clone())
            .await
            .unwrap();
        let mut session = open(child, lines, spec()).await.unwrap();
        assert_eq!(session.native_id(), Some(NativeId::new("vendor-thread")));
        let evidence = session.identity_evidence().await;
        assert_eq!(evidence.len(), 1);
        {
            let records = capture.records.lock().unwrap();
            let (reply, _) = records
                .iter()
                .find(|(_, observation)| observation == &evidence[0])
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&reply.bytes).unwrap()["result"]["thread"]["id"],
                "vendor-thread"
            );
            assert!(records
                .iter()
                .any(|(record, _)| record.stream == reply.stream
                    && record.transport == sylph_protocol::trace::TransportKind::LiveStdin
                    && serde_json::from_slice::<Value>(&record.bytes)
                        .is_ok_and(|value| value["method"] == "thread/start" && value["id"] == 3)));
        }
        assert!(session.messages().is_some());
        assert!(session.messages().is_none());
        session.close().await.unwrap();
    }

    #[tokio::test]
    async fn controls_ignore_other_threads_and_late_completion_of_a_previous_turn() {
        let state = Arc::new(Mutex::new(State {
            thread: Some("owned".into()),
            ..Default::default()
        }));
        let router = ControlRouter {
            state: state.clone(),
            settings_echo: Arc::new(Notify::new()),
        };
        assert!(router.frame(&json!({"method":"turn/started","params":{"threadId":"child","turn":{"id":"foreign"}}})).await.is_empty());
        let active = router
            .frame(
                &json!({"method":"turn/started","params":{"threadId":"owned","turn":{"id":"new"}}}),
            )
            .await;
        assert!(
            matches!(active.as_slice(),[Ok(SessionMessage::Control(ControlNotice::Active {native_turn_id:Some(turn),..}))] if turn=="new")
        );
        assert!(router.frame(&json!({"method":"turn/completed","params":{"threadId":"owned","turn":{"id":"old"}}})).await.is_empty());
        assert_eq!(state.lock().await.turn.as_deref(), Some("new"));
        assert!(router.frame(&json!({"method":"thread/settings/updated","params":{"threadId":"child","threadSettings":{"model":"foreign"}}})).await.is_empty());
        assert!(state.lock().await.settings.is_none());
        let settled=router.frame(&json!({"method":"turn/completed","params":{"threadId":"owned","turn":{"id":"new"}}})).await;
        assert!(matches!(
            settled.as_slice(),
            [Ok(SessionMessage::Control(ControlNotice::Settled { .. }))]
        ));
        assert!(state.lock().await.turn.is_none());
    }

    #[tokio::test]
    async fn approval_choices_preserve_request_authority_and_foreign_requests_are_not_controlled() {
        let state = Arc::new(Mutex::new(State {
            thread: Some("owned".into()),
            ..Default::default()
        }));
        let router = ControlRouter {
            state: state.clone(),
            settings_echo: Arc::new(Notify::new()),
        };
        let decisions = json!(["accept",{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["echo","hello"]}},"cancel"]);
        let mut frame = json!({"method":"item/commandExecution/requestApproval","id":0,"params":{"threadId":"foreign","turnId":"turn","itemId":"exec","command":"echo hello","availableDecisions":decisions}});
        assert!(router.frame(&frame).await.is_empty());
        assert!(state.lock().await.pending.is_empty());
        frame["params"]["threadId"] = json!("owned");
        let requests = router.frame(&frame).await;
        let [Ok(SessionMessage::Request(request))] = requests.as_slice() else {
            panic!("approval request missing")
        };
        assert_eq!(request.id, "0");
        assert_eq!(request.tool_use_id.as_deref(), Some("exec"));
        assert_eq!(
            request
                .options
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
            vec!["accept", "acceptWithExecpolicyAmendment", "cancel"]
        );
        assert_eq!(
            state.lock().await.pending["0"],
            decisions.as_array().unwrap().clone()
        );
        router
            .frame(&json!({"method":"serverRequest/resolved","params":{"requestId":0}}))
            .await;
        assert!(state.lock().await.pending.is_empty());
    }
}
