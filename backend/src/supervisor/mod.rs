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
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::db::LiveStore;
use crate::env::{Environment, ProcessHandle, ProcessSignal, ProcessSpec, Signal};
use crate::events::{EventHub, RunnerEnvelope, RunnerStreamLine, SeqAllocator, StreamEvent};
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
    /// Signal channel for the running process, held rather than a pid because
    /// the environment decides what addressing a signal even means — a pgid
    /// locally, an RPC by sandbox and process id in Khora.
    signaler: Arc<dyn ProcessSignal>,
    /// Set when a stop was requested, so the outcome is reported as `cancelled`
    /// rather than a runner failure.
    stop_requested: bool,
}


#[derive(Clone)]
pub struct TurnSupervisor {
    store: LiveStore,
    registry: Arc<Mutex<Registry>>,
    hub: EventHub,
    active: Arc<RwLock<HashMap<String, ActiveTurn>>>,
    /// Shared with the link drain task, because both emit events for one session
    /// and separate counters would collide on `UNIQUE(session_id, seq)`.
    seq: SeqAllocator,
    soul_path: Option<PathBuf>,
    /// Where turns run. One environment today; a session will name its own once
    /// sandboxes land, which is why this is a trait object from the start rather
    /// than a concrete type to be generalized later.
    environment: Arc<dyn Environment>,
}

impl TurnSupervisor {
    pub fn new(
        store: LiveStore,
        registry: Arc<Mutex<Registry>>,
        hub: EventHub,
        soul_path: Option<PathBuf>,
        environment: Arc<dyn Environment>,
        seq: SeqAllocator,
    ) -> Self {
        Self {
            store,
            registry,
            hub,
            active: Arc::new(RwLock::new(HashMap::new())),
            seq,
            soul_path,
            environment,
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
    pub async fn stop(&self, turn_id: &str) -> crate::error::Result<bool> {
        let signaler = {
            let mut active = self.active.write().await;
            match active.get_mut(turn_id) {
                Some(turn) => {
                    turn.stop_requested = true;
                    turn.signaler.clone()
                }
                None => return Ok(false),
            }
        };

        info!("Stopping turn {}", turn_id);
        if let Err(error) = signaler.signal(Signal::Term).await {
            debug!("SIGTERM for turn {} failed: {}", turn_id, error);
        }

        let active = self.active.clone();
        let turn_id = turn_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(STOP_GRACE).await;
            // Still registered means the process ignored SIGTERM.
            if active.read().await.contains_key(&turn_id) {
                warn!("Turn {} survived SIGTERM; sending SIGKILL", turn_id);
                let _ = signaler.signal(Signal::Kill).await;
            }
        });

        Ok(true)
    }

    /// Assign a sequence number, journal the event if stable, and publish it.
    async fn emit(&self, session_id: &str, turn_id: &str, mut event: StreamEvent) -> crate::error::Result<()> {
        event.seq = self.seq.next(session_id).await?;

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

        let mut args: Vec<String> = vec!["--workdir".into(), workdir.clone()];

        if let Some(sid) = backend_session_id.as_deref() {
            args.push("--session-id".into());
            args.push(sid.to_string());
        }
        for (key, value) in &flags {
            args.push("--flag".into());
            args.push(format!("{}={}", key, value));
        }
        for path in &attachments {
            args.push("--attachment".into());
            args.push(path.to_string_lossy().into_owned());
        }
        if let Some(command) = command.as_deref() {
            args.push("--command".into());
            args.push(command.to_string());
            for (key, value) in &command_args {
                args.push("--arg".into());
                args.push(format!("{}={}", key, value));
            }
        }

        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("SYLPH_STREAM_FORMAT".into(), "ndjson-v1".into());

        // Identity injection is the runner's job, but whether it happens at all
        // is ours: native sessions must never receive SOUL.md, and the protocol
        // reserves the empty string for exactly that.
        match (inject_soul, self.soul_path.as_ref()) {
            (true, Some(path)) => {
                env.insert("SYLPH_SOUL".into(), path.to_string_lossy().into_owned());
            }
            (true, None) => {} // Let the runner resolve its default location.
            (false, _) => {
                env.insert("SYLPH_SOUL".into(), String::new());
            }
        }

        let mut child = self
            .environment
            .spawn(ProcessSpec {
                program: runner_path.clone(),
                args,
                cwd: workdir.clone(),
                env,
                // A v1 runner learns the prompt ended from EOF, so its stdin is
                // written once and dropped. This is the flag the v2 adapter
                // protocol inverts.
                duplex_stdin: false,
            })
            .await
            .with_context(|| {
                format!("Failed to spawn runner {:?} for agent '{}'", runner_path, agent)
            })?;

        self.active.write().await.insert(
            turn_id.clone(),
            ActiveTurn {
                session_id: session_id.clone(),
                signaler: child.signaler(),
                stop_requested: false,
            },
        );

        // Feed the prompt and close stdin: v1 runners read to EOF.
        if let Some(mut stdin) = child.take_stdin() {
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
        let exit_code = child.wait().await.context("Failed to await runner exit")?;
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
    fn drain_stderr(child: &mut Box<dyn ProcessHandle>) -> tokio::task::JoinHandle<String> {
        let stderr = child.take_stderr();
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
        child: &mut Box<dyn ProcessHandle>,
        session_id: &str,
        turn_id: &str,
    ) -> Option<RunnerEnvelope> {
        let stdout = child.take_stdout()?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{RunnerEvent, RunnerStreamLine};

    /// Event kinds the journal's CHECK constraint accepts, copied from
    /// `migrations/001_initial_schema.sql`. Kept here so a schema change that
    /// drops a kind fails a test instead of aborting a live turn.
    const JOURNAL_KINDS: &[&str] = &[
        "status",
        "system",
        "user",
        "thinking",
        "assistant",
        "tool_call",
        "tool_result",
        "usage",
        "error",
        "approval",
    ];

    fn line(kind: &str, phase: Option<&str>) -> RunnerStreamLine {
        RunnerStreamLine {
            stream: "sylph.runner.v1".to_string(),
            event: RunnerEvent {
                kind: kind.to_string(),
                phase: phase.map(str::to_string),
                id: None,
                name: None,
                text: None,
                data: None,
                is_error: None,
            },
        }
    }

    #[test]
    fn every_protocol_kind_survives_normalization_unchanged() {
        for kind in JOURNAL_KINDS {
            let normalized = normalize(line(kind, Some("completed")));
            assert_eq!(
                &normalized.kind, kind,
                "{kind} is a journal kind and must not be coerced"
            );
        }
    }

    #[test]
    fn an_unknown_kind_is_coerced_to_status() {
        // A newer runner emitting a kind this build has never seen must not be
        // able to abort the turn on the journal's CHECK constraint.
        for unknown in ["reasoning_summary", "", "STATUS", "tool-call"] {
            let normalized = normalize(line(unknown, Some("completed")));
            assert_eq!(normalized.kind, "status", "{unknown:?} should coerce");
            assert!(JOURNAL_KINDS.contains(&normalized.kind.as_str()));
        }
    }

    #[test]
    fn stability_follows_phase_and_matches_is_stable() {
        // normalize() and StreamEvent::is_stable() decide durability
        // independently. If they disagree, an event is written with a stability
        // its phase contradicts.
        for (phase, expected) in [
            (Some("completed"), "stable"),
            (None, "stable"),
            (Some("started"), "ephemeral"),
            (Some("delta"), "ephemeral"),
            (Some("unrecognized"), "ephemeral"),
        ] {
            let normalized = normalize(line("assistant", phase));
            assert_eq!(normalized.stability, expected, "phase {phase:?}");
            assert_eq!(
                normalized.is_stable(),
                expected == "stable",
                "phase {phase:?} disagrees with is_stable()"
            );
        }
    }

    #[test]
    fn absent_optional_fields_normalize_to_their_empty_forms() {
        let normalized = normalize(line("assistant", Some("delta")));
        assert_eq!(normalized.text, "", "absent text is empty, not null");
        assert!(normalized.data.is_none());
        assert!(!normalized.is_error, "absent is_error defaults to false");
        assert!(normalized.stream_id.is_none());
        assert!(normalized.name.is_none());
    }

    #[test]
    fn the_runner_block_id_becomes_the_stream_id() {
        // The protocol calls it `id`; the journal calls it stream_id so it
        // cannot be confused with a row id.
        let mut raw = line("tool_call", Some("completed"));
        raw.event.id = Some("blk-7".to_string());
        raw.event.name = Some("Read".to_string());
        raw.event.text = Some("body".to_string());
        raw.event.is_error = Some(true);
        raw.event.data = Some(serde_json::json!({"path": "/tmp/x"}));

        let normalized = normalize(raw);

        assert_eq!(normalized.stream_id.as_deref(), Some("blk-7"));
        assert_eq!(normalized.name.as_deref(), Some("Read"));
        assert_eq!(normalized.text, "body");
        assert!(normalized.is_error);
        assert_eq!(normalized.data, Some(serde_json::json!({"path": "/tmp/x"})));
    }

    #[test]
    fn normalized_events_are_attributed_to_the_runner_with_an_unassigned_seq() {
        // The session-wide counter assigns seq at persist time, so normalize
        // must leave it at 0 rather than inventing an ordering.
        let normalized = normalize(line("assistant", Some("completed")));
        assert_eq!(normalized.source, "runner");
        assert_eq!(normalized.seq, 0);
        assert!(normalized.created_at_ms > 0);
    }

    #[test]
    fn a_runner_event_accepts_both_spellings_of_is_error() {
        for raw in [
            r#"{"kind":"tool_result","phase":"completed","is_error":true}"#,
            r#"{"kind":"tool_result","phase":"completed","isError":true}"#,
        ] {
            let event: RunnerEvent = serde_json::from_str(raw).unwrap();
            assert_eq!(event.is_error, Some(true), "failed on {raw}");
        }
    }

    #[test]
    fn a_runner_event_tolerates_unknown_fields() {
        // A newer runner must be able to add fields without breaking this
        // consumer.
        let event: RunnerEvent = serde_json::from_str(
            r#"{"kind":"assistant","phase":"delta","text":"hi","future_field":42}"#,
        )
        .unwrap();
        assert_eq!(event.kind, "assistant");
        assert_eq!(event.text.as_deref(), Some("hi"));
    }

    #[test]
    fn server_authored_events_are_stable_and_phaseless() {
        let event = TurnSupervisor::server_event("error", "boom".to_string(), None, true);
        assert_eq!(event.source, "server");
        assert_eq!(event.stability, "stable");
        assert!(event.phase.is_none(), "server events carry no phase");
        assert!(event.is_stable());
        assert!(event.is_error);
        assert_eq!(event.text, "boom");
    }
}
