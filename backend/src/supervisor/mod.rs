//! Turn supervision: spawning runner processes and normalizing their output.
//!
//! A turn is a durable resource whose lifetime is independent of the HTTP
//! request that created it (`docs/architecture.md`, "Turn Execution Model").
//! `POST /turns` records the turn and returns `202`; this module then drives it
//! to completion in a detached task, publishing events through [`EventHub`] so
//! any number of SSE clients can attach, detach, and reattach meanwhile.
//!
//! The wire contract is `references/runner-protocol.md`:
//!
//! * The runner is invoked as
//!   `runner.sh --workdir DIR [--session-id SID] [--flag K=V]... [--attachment PATH]...`
//!   with the prompt on stdin.
//! * With `SYLPH_STREAM_FORMAT=ndjson-v1` set, it emits `sylph.runner.v1` JSON
//!   lines as it works.
//! * The **last non-empty stdout line** is the authoritative envelope. The
//!   protocol says to trust it over the exit code, so a well-formed
//!   `{"ok": true, ...}` marks the turn completed even on a non-zero exit.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::db::LiveStore;
use crate::events::{EventHub, RunnerEnvelope, RunnerStreamLine, StreamEvent};
use crate::runners::Registry;

/// Grace period between SIGTERM and SIGKILL on stop, matching the runner
/// protocol's documented process-group semantics.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Everything needed to execute one turn.
pub struct TurnRequest {
    pub session_id: String,
    pub turn_id: String,
    pub incarnation_id: String,
    pub agent: String,
    pub workdir: String,
    /// Agent-native session id to resume. `None` on an incarnation's first turn.
    pub backend_session_id: Option<String>,
    pub prompt: String,
    pub flags: HashMap<String, String>,
    pub attachments: Vec<PathBuf>,
    /// Named command from `--describe`, if this is a command invocation.
    pub command: Option<String>,
    pub command_args: HashMap<String, String>,
    /// Whether to inject SOUL.md. False for native sessions, whose identity is
    /// preserved — a product invariant, not a preference.
    pub inject_soul: bool,
}

/// A live child process, tracked so `stop` can signal it.
struct ActiveTurn {
    session_id: String,
    /// Process-group id, equal to the child's pid because the child is spawned
    /// into a new group. Signaling the group reaches the CLI the runner exec'd.
    pgid: i32,
    /// Set when a stop was requested, so the outcome is reported as `cancelled`
    /// rather than a runner failure.
    stop_requested: bool,
}

/// Per-session monotonic event sequence counter.
///
/// Seeded from the journal so sequence numbers keep climbing across restarts
/// and never collide with the `UNIQUE(session_id, seq)` constraint.
type SeqCounters = Arc<RwLock<HashMap<String, Arc<AtomicI32>>>>;

#[derive(Clone)]
pub struct TurnSupervisor {
    store: LiveStore,
    registry: Arc<Mutex<Registry>>,
    hub: EventHub,
    active: Arc<RwLock<HashMap<String, ActiveTurn>>>,
    seq_counters: SeqCounters,
    soul_path: Option<PathBuf>,
}

impl TurnSupervisor {
    pub fn new(
        store: LiveStore,
        registry: Arc<Mutex<Registry>>,
        hub: EventHub,
        soul_path: Option<PathBuf>,
    ) -> Self {
        Self {
            store,
            registry,
            hub,
            active: Arc::new(RwLock::new(HashMap::new())),
            seq_counters: Arc::new(RwLock::new(HashMap::new())),
            soul_path,
        }
    }

    /// Whether a session currently has a running turn.
    pub async fn session_busy(&self, session_id: &str) -> bool {
        self.active
            .read()
            .await
            .values()
            .any(|turn| turn.session_id == session_id)
    }

    pub async fn active_turn_count(&self) -> usize {
        self.active.read().await.len()
    }

    /// Launch a turn in a detached task and return immediately.
    ///
    /// The task owns the turn's outcome from here on: it records terminal state
    /// in the journal whatever happens, including on spawn failure, so a turn
    /// can never be left permanently `running`.
    pub async fn spawn(&self, request: TurnRequest) {
        let supervisor = self.clone();
        tokio::spawn(async move {
            let turn_id = request.turn_id.clone();
            let session_id = request.session_id.clone();
            if let Err(error) = supervisor.run_turn(request).await {
                error!("Turn {} failed: {:#}", turn_id, error);
                supervisor
                    .fail_turn(&session_id, &turn_id, "RUNNER_START_FAILED", &format!("{:#}", error))
                    .await;
            }
        });
    }

    /// Request a stop: SIGTERM the process group, then SIGKILL after the grace
    /// period if it is still alive.
    ///
    /// Returns false when the turn has no live process, in which case the caller
    /// reports `STOP_ALREADY_COMPLETED`.
    pub async fn stop(&self, turn_id: &str) -> Result<bool> {
        let pgid = {
            let mut active = self.active.write().await;
            match active.get_mut(turn_id) {
                Some(turn) => {
                    turn.stop_requested = true;
                    turn.pgid
                }
                None => return Ok(false),
            }
        };

        info!("Stopping turn {} (pgid {})", turn_id, pgid);
        // ESRCH here means the group already exited between the lookup and the
        // signal, which is a benign race — the reaper will record the outcome.
        if let Err(error) = killpg(Pid::from_raw(pgid), Signal::SIGTERM) {
            debug!("SIGTERM to pgid {} failed: {}", pgid, error);
        }

        let active = self.active.clone();
        let turn_id = turn_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(STOP_GRACE).await;
            // Still registered means the process ignored SIGTERM.
            if active.read().await.contains_key(&turn_id) {
                warn!("Turn {} survived SIGTERM; sending SIGKILL", turn_id);
                let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
            }
        });

        Ok(true)
    }

    /// Next sequence number for a session, seeded from the journal on first use.
    async fn next_seq(&self, session_id: &str) -> Result<i32> {
        let counter = {
            let counters = self.seq_counters.read().await;
            counters.get(session_id).cloned()
        };

        let counter = match counter {
            Some(counter) => counter,
            None => {
                let max = self.store.max_event_seq(session_id).await?;
                let mut counters = self.seq_counters.write().await;
                // Another task may have seeded this while we queried; keep
                // whichever counter landed first so numbering stays monotonic.
                counters
                    .entry(session_id.to_string())
                    .or_insert_with(|| Arc::new(AtomicI32::new(max + 1)))
                    .clone()
            }
        };

        Ok(counter.fetch_add(1, Ordering::SeqCst))
    }

    /// Assign a sequence number, journal the event if stable, and publish it.
    async fn emit(&self, session_id: &str, turn_id: &str, mut event: StreamEvent) -> Result<()> {
        event.seq = self.next_seq(session_id).await?;

        if event.is_stable() {
            // Journal before publishing so a client that reconnects
            // immediately after seeing an event can always find it in replay.
            if let Err(error) = self.store.append_event(&event.to_row(session_id, turn_id)).await {
                error!("Failed to journal event seq {}: {}", event.seq, error);
            }
        }

        self.hub.publish(turn_id, event).await;
        Ok(())
    }

    /// Build a server-authored event. These have no phase and are always stable.
    fn server_event(kind: &str, text: String, data: Option<serde_json::Value>, is_error: bool) -> StreamEvent {
        StreamEvent {
            seq: 0,
            kind: kind.to_string(),
            phase: None,
            stream_id: None,
            name: None,
            text,
            data,
            is_error,
            stability: "stable".to_string(),
            source: "server".to_string(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Record a terminal failure for a turn whose process never produced a
    /// usable outcome.
    async fn fail_turn(&self, session_id: &str, turn_id: &str, code: &str, message: &str) {
        let _ = self
            .emit(
                session_id,
                turn_id,
                Self::server_event("error", message.to_string(), Some(serde_json::json!({"code": code})), true),
            )
            .await;
        let _ = self
            .store
            .finish_turn(turn_id, "failed", None, Some(code), Some(message))
            .await;
        let _ = self.store.update_session_status(session_id, "idle").await;
        self.hub.close(turn_id).await;
        self.active.write().await.remove(turn_id);
    }

    /// Drive one turn from spawn to terminal state.
    async fn run_turn(&self, request: TurnRequest) -> Result<()> {
        let TurnRequest {
            session_id,
            turn_id,
            incarnation_id,
            agent,
            workdir,
            backend_session_id,
            prompt,
            flags,
            attachments,
            command,
            command_args,
            inject_soul,
        } = request;

        let runner_path = {
            let registry = self.registry.lock().await;
            registry
                .get_runner_path(&agent)
                .map(|path| path.to_path_buf())
                .with_context(|| format!("No runner registered for agent '{}'", agent))?
        };

        self.hub.open(&turn_id).await;

        if !self.store.mark_turn_running(&turn_id, &incarnation_id).await? {
            // Already claimed, stopped, or interrupted — nothing to do.
            debug!("Turn {} was no longer queued; skipping execution", turn_id);
            self.hub.close(&turn_id).await;
            return Ok(());
        }
        self.store.update_session_status(&session_id, "running").await?;

        // The user's own message is part of the durable transcript, so it is
        // journaled as an event rather than living only in `turns.request_text`.
        if !prompt.is_empty() {
            self.emit(
                &session_id,
                &turn_id,
                Self::server_event("user", prompt.clone(), None, false),
            )
            .await?;
        }

        self.emit(
            &session_id,
            &turn_id,
            Self::server_event(
                "status",
                format!("Starting {}", agent),
                Some(serde_json::json!({"state": "started", "agent": agent})),
                false,
            ),
        )
        .await?;

        let mut cmd = Command::new(&runner_path);
        cmd.arg("--workdir").arg(&workdir);

        if let Some(sid) = backend_session_id.as_deref() {
            cmd.arg("--session-id").arg(sid);
        }
        for (key, value) in &flags {
            cmd.arg("--flag").arg(format!("{}={}", key, value));
        }
        for path in &attachments {
            cmd.arg("--attachment").arg(path);
        }
        if let Some(command) = command.as_deref() {
            cmd.arg("--command").arg(command);
            for (key, value) in &command_args {
                cmd.arg("--arg").arg(format!("{}={}", key, value));
            }
        }

        cmd.current_dir(&workdir)
            .env("SYLPH_STREAM_FORMAT", "ndjson-v1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A new process group means SIGTERM on stop reaches the CLI the
            // runner exec'd, not just the wrapper script.
            .process_group(0);

        // Identity injection is the runner's job, but whether it happens at all
        // is ours: native sessions must never receive SOUL.md, and the protocol
        // reserves the empty string for exactly that.
        match (inject_soul, self.soul_path.as_ref()) {
            (true, Some(path)) => {
                cmd.env("SYLPH_SOUL", path);
            }
            (true, None) => {} // Let the runner resolve its default location.
            (false, _) => {
                cmd.env("SYLPH_SOUL", "");
            }
        }

        let mut child = cmd.spawn().with_context(|| {
            format!("Failed to spawn runner {:?} for agent '{}'", runner_path, agent)
        })?;

        let pid = child.id().context("Runner exited before its pid was available")?;
        self.active.write().await.insert(
            turn_id.clone(),
            ActiveTurn {
                session_id: session_id.clone(),
                // The child leads its own group, so pgid == pid.
                pgid: pid as i32,
                stop_requested: false,
            },
        );

        // Feed the prompt and close stdin: runners read to EOF.
        if let Some(mut stdin) = child.stdin.take() {
            let prompt_bytes = prompt.clone().into_bytes();
            tokio::spawn(async move {
                if let Err(error) = stdin.write_all(&prompt_bytes).await {
                    debug!("Writing prompt to runner stdin failed: {}", error);
                }
                let _ = stdin.shutdown().await;
            });
        }

        let stderr_tail = Self::drain_stderr(&mut child);
        let outcome = self.consume_stdout(&mut child, &session_id, &turn_id).await;
        let status = child.wait().await.context("Failed to await runner exit")?;
        let exit_code = status.code();
        let stderr_tail = stderr_tail.await.unwrap_or_default();

        let stop_requested = self
            .active
            .write()
            .await
            .remove(&turn_id)
            .is_some_and(|turn| turn.stop_requested);

        self.finalize(FinalizeArgs {
            session_id: &session_id,
            turn_id: &turn_id,
            incarnation_id: &incarnation_id,
            outcome,
            exit_code,
            stderr_tail,
            stop_requested,
            is_command: command.is_some(),
        })
        .await
    }

    /// Collect stderr concurrently with stdout so a chatty runner cannot fill
    /// the stderr pipe and deadlock. Only the tail is kept, for error reporting.
    fn drain_stderr(child: &mut Child) -> tokio::task::JoinHandle<String> {
        let stderr = child.stderr.take();
        tokio::spawn(async move {
            const TAIL_LIMIT: usize = 4096;
            let Some(stderr) = stderr else {
                return String::new();
            };
            let mut reader = BufReader::new(stderr).lines();
            let mut tail = String::new();
            while let Ok(Some(line)) = reader.next_line().await {
                tail.push_str(&line);
                tail.push('\n');
                if tail.len() > TAIL_LIMIT * 2 {
                    tail = tail.split_off(tail.len() - TAIL_LIMIT);
                }
            }
            tail.trim().to_string()
        })
    }

    /// Read the runner's stdout line by line, publishing normalized events and
    /// remembering the last line that parsed as the final envelope.
    ///
    /// Non-JSON lines and JSON that is neither a stream line nor an envelope are
    /// diagnostics: the protocol allows them, so they are logged and skipped.
    async fn consume_stdout(
        &self,
        child: &mut Child,
        session_id: &str,
        turn_id: &str,
    ) -> Option<RunnerEnvelope> {
        let Some(stdout) = child.stdout.take() else {
            return None;
        };

        let mut lines = BufReader::new(stdout).lines();
        let mut envelope = None;

        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                debug!("Runner diagnostic line: {}", trimmed);
                continue;
            };

            // A stream line is tagged; the envelope is identified by `ok`.
            if value.get("stream").and_then(|s| s.as_str()) == Some("sylph.runner.v1") {
                match serde_json::from_value::<RunnerStreamLine>(value) {
                    Ok(line) => {
                        let event = normalize(line);
                        if let Err(error) = self.emit(session_id, turn_id, event).await {
                            error!("Failed to emit runner event: {}", error);
                        }
                    }
                    Err(error) => warn!("Malformed sylph.runner.v1 line: {}", error),
                }
                continue;
            }

            if value.get("ok").is_some_and(|ok| ok.is_boolean()) {
                match serde_json::from_value::<RunnerEnvelope>(value) {
                    // Keep the last one: the protocol makes the final line
                    // authoritative, and a runner may print more after this.
                    Ok(parsed) => envelope = Some(parsed),
                    Err(error) => warn!("Malformed runner envelope: {}", error),
                }
                continue;
            }

            debug!("Unrecognized runner stdout line: {}", trimmed);
        }

        envelope
    }

    /// Record the turn's terminal state and publish a closing status event.
    async fn finalize(&self, args: FinalizeArgs<'_>) -> Result<()> {
        let FinalizeArgs {
            session_id,
            turn_id,
            incarnation_id,
            outcome,
            exit_code,
            stderr_tail,
            stop_requested,
            is_command,
        } = args;

        // Persisting the returned session id is what makes the next turn a
        // continuation rather than a fresh context.
        if let Some(sid) = outcome.as_ref().and_then(|envelope| envelope.session_id.as_deref()) {
            if !sid.is_empty() {
                if let Err(error) = self
                    .store
                    .set_backend_session_id(session_id, Some(incarnation_id), sid)
                    .await
                {
                    error!("Failed to persist backend session id: {}", error);
                }
            }
        }

        let (status, error_code, message) = match (&outcome, stop_requested) {
            // A stop that raced a successful finish still counts as cancelled:
            // the user asked for it and the reply may be partial.
            (_, true) => ("cancelled", Some("TURN_INTERRUPTED"), "Stopped by user".to_string()),
            (Some(envelope), false) if envelope.ok => ("completed", None, String::new()),
            (Some(envelope), false) => (
                "failed",
                Some("TURN_FAILED"),
                envelope
                    .error
                    .clone()
                    .unwrap_or_else(|| "Runner reported failure without a message".to_string()),
            ),
            // No envelope at all is a protocol violation; stderr is the only
            // diagnostic available, so surface its tail.
            (None, false) => (
                "failed",
                Some("RUNNER_PROTOCOL_ERROR"),
                if stderr_tail.is_empty() {
                    format!(
                        "Runner produced no final JSON envelope (exit code {})",
                        exit_code.map_or_else(|| "signal".to_string(), |c| c.to_string())
                    )
                } else {
                    format!("Runner produced no final JSON envelope: {}", stderr_tail)
                },
            ),
        };

        // The assistant reply lives in the envelope, not the event stream: the
        // protocol notes that emitting it as an event too would persist the
        // message twice. It is journaled here only when the stream did not
        // already carry a completed assistant block.
        if status == "completed" {
            if let Some(text) = outcome.as_ref().and_then(|envelope| envelope.text.as_deref()) {
                if !text.is_empty() {
                    let already_streamed = self
                        .store
                        .turn_events_after(turn_id, -1)
                        .await
                        .map(|events| {
                            events.iter().any(|event| {
                                event.kind == "assistant" && event.phase.as_deref() == Some("completed")
                            })
                        })
                        .unwrap_or(false);

                    if !already_streamed {
                        // Command invocations return an ephemeral status line
                        // rather than an assistant message.
                        let kind = if is_command { "status" } else { "assistant" };
                        self.emit(
                            session_id,
                            turn_id,
                            Self::server_event(kind, text.to_string(), None, false),
                        )
                        .await?;
                    }
                }
            }
        }

        if !message.is_empty() {
            self.emit(
                session_id,
                turn_id,
                Self::server_event(
                    "error",
                    message.clone(),
                    Some(serde_json::json!({"code": error_code})),
                    true,
                ),
            )
            .await?;
        }

        self.emit(
            session_id,
            turn_id,
            Self::server_event(
                "status",
                String::new(),
                Some(serde_json::json!({"state": status, "exitCode": exit_code})),
                false,
            ),
        )
        .await?;

        self.store
            .finish_turn(
                turn_id,
                status,
                exit_code,
                error_code,
                if message.is_empty() { None } else { Some(message.as_str()) },
            )
            .await?;
        self.store.update_session_status(session_id, "idle").await?;
        self.store.touch_session(session_id).await?;

        self.hub.close(turn_id).await;
        info!("Turn {} finished with status {}", turn_id, status);
        Ok(())
    }
}

/// Grouped arguments for [`TurnSupervisor::finalize`].
struct FinalizeArgs<'a> {
    session_id: &'a str,
    turn_id: &'a str,
    incarnation_id: &'a str,
    outcome: Option<RunnerEnvelope>,
    exit_code: Option<i32>,
    stderr_tail: String,
    stop_requested: bool,
    is_command: bool,
}

/// Map a runner stream line onto the normalized event shape.
///
/// The runner protocol's `kind` vocabulary is narrower than the journal's, and
/// `phase` decides durability: `completed` is the stable unit of record, while
/// `started` and `delta` are transient projections.
fn normalize(line: RunnerStreamLine) -> StreamEvent {
    let event = line.event;
    let stability = match event.phase.as_deref() {
        Some("completed") | None => "stable",
        _ => "ephemeral",
    };

    // Unknown kinds are coerced to `status` so a newer runner cannot violate
    // the journal's CHECK constraint and abort the turn.
    let kind = match event.kind.as_str() {
        known @ ("status" | "system" | "user" | "thinking" | "assistant" | "tool_call"
        | "tool_result" | "usage" | "error" | "approval") => known,
        other => {
            debug!("Coercing unknown runner event kind '{}' to status", other);
            "status"
        }
    };

    StreamEvent {
        seq: 0,
        kind: kind.to_string(),
        phase: event.phase,
        stream_id: event.id,
        name: event.name,
        text: event.text.unwrap_or_default(),
        data: event.data,
        is_error: event.is_error.unwrap_or(false),
        stability: stability.to_string(),
        source: "runner".to_string(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
    }
}
