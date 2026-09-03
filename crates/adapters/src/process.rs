//! A held vendor process, framed as lines of JSON.
//!
//! All three agents speak newline-delimited JSON over stdio — `claude` as
//! `--input-format stream-json`, `pi` as `--mode rpc`, `codex` as `app-server` JSON-RPC —
//! so the framing, the reply correlation, and the shutdown escalation are one
//! implementation rather than three. What differs per vendor is the *content* of a line,
//! which is each module's `translate`.
//!
//! # Two things this owns that a caller must not re-derive
//!
//! **The process group.** A vendor CLI spawns children — a shell for a tool call, an MCP
//! server — and signalling only the parent leaves them running. [`Child::spawn`] puts the
//! process in its own group and [`Child::close`] signals the group.
//!
//! **The read loop's ownership of stdout.** Lines are read once, into a channel, by a task
//! that outlives any single call. A second reader would see half the turn, which is the
//! same reason [`sylph_protocol::Session::events`] yields its stream once.

use std::collections::HashMap;
use std::ops::Deref;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::debug;

/// How long a request may wait for its reply before the vendor is called unresponsive.
///
/// **Not a bound on how long an agent may think.** A turn is accepted, not awaited: the
/// outcome arrives as an event. This bounds only a correlated request — `get_commands`,
/// `set_model` — where a vendor that never answers would otherwise wedge the caller
/// holding the lock.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a vendor gets to exit on its own before the group is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// How much stderr to keep, so a spawn failure can say why.
///
/// A vendor writes its diagnostics there and nowhere else, so an adapter that discards it
/// reports "the process exited" for a missing API key, an unwritable config, and a
/// panic alike.
const STDERR_TAIL_BYTES: usize = 4096;

/// One parsed line the vendor wrote to stdout, retaining its lexical payload.
///
/// `value` is what adapters normally consume. `raw` is the exact line bytes, excluding only
/// the framing newline, so a recorder can preserve whitespace, key order, duplicate keys, and
/// escape spelling. A parse failure is [`Err`] and **does not end the stream**: one malformed
/// line must not fail a turn, and a vendor that has just started may print a banner before its
/// protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessLine {
    pub value: Value,
    pub raw: Vec<u8>,
}

impl Deref for ProcessLine {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

pub type Line = Result<ProcessLine>;

/// A held vendor process.
pub struct Child {
    /// Serialized writes.
    ///
    /// A channel rather than a shared `AsyncWrite`, because two concurrent writers can
    /// interleave halves of two JSON lines — which corrupts the stream in a way the vendor
    /// reports as a parse error about content neither writer sent.
    stdin: mpsc::Sender<String>,
    /// Replies awaiting their caller, keyed by the id we sent.
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    /// Monotonic request ids, unique for this process's life.
    next_id: AtomicU64,
    /// The process group, addressable while another task drains stdout.
    pgid: i32,
    /// stderr as it accumulated, for an error that has to say why.
    stderr: Arc<Mutex<String>>,
    /// Whether the vendor's stdout has ended, so a later request fails instead of waiting.
    ///
    /// **A write to a dead process succeeds**, which is what makes this necessary rather
    /// than tidy: the writer task is alive and the OS buffers a write to a pipe whose reader
    /// is gone. So [`Self::request`] cannot learn from the write that nobody will answer.
    ///
    /// Without this the failure is a *timeout*: the reader task is what drains the pending
    /// map on EOF, and it has already exited, so a request registered afterwards waits the
    /// full [`REPLY_TIMEOUT`] and then reports "the vendor did not answer within 30s". That
    /// is the wrong sentence for a process that is not running, and thirty seconds is a long
    /// time to spend arriving at it — measured, a request after EOF blocked for the whole
    /// budget. Any control after a session's process dies would pay it.
    closed: Arc<AtomicBool>,
}

/// How to start one vendor process.
pub struct Spawn {
    /// The program name, resolved through `PATH`.
    pub program: String,
    pub args: Vec<String>,
    /// The agent's `current_dir`, already contained by whoever named it.
    pub cwd: String,
    /// Added to the inherited environment rather than replacing it: a vendor CLI needs
    /// `HOME`, `PATH`, and its own credential variables to work at all.
    pub env: HashMap<String, String>,
    /// How to recognize a **reply** and read the id it answers.
    ///
    /// Answers `Some(id)` only for a frame that is a reply to something this side sent,
    /// and `None` for everything else — an event, or a request the *vendor* initiated.
    /// `None` for the whole field means the vendor's stdout carries no replies, in which
    /// case [`Child::request`] is unavailable and every line reaches the stream.
    ///
    /// # Why a closure and not a field name
    ///
    /// A field name cannot express either half of this, and both halves are defects that
    /// were measured rather than imagined.
    ///
    /// **Direction is not in the id.** `codex` is JSON-RPC, and its server→client requests
    /// are numbered in the *same* space as ours: measured in
    /// `docs/measurements/codex/captures/s15_approval_decisions.jsonl`, approvals arrive as
    /// `item/commandExecution/requestApproval` with ids `0, 1, 2` while our own requests hold
    /// `1..5`. Correlating on the id alone therefore routes an **approval request** into
    /// whichever waiter happens to hold that number: the request never reaches the event
    /// stream, so nobody is asked, and `set_config` returns an approval as its reply. Both
    /// directions of that failure are silent. JSON-RPC's own discriminant is `method` —
    /// present on a request, absent on a reply — which is a fact about that protocol and
    /// not about a field called `id`.
    ///
    /// **The id is not always at the top level.** `claude` nests it: a reply is
    /// `{"type": "control_response", "response": {"request_id": …}}`, so any top-level
    /// lookup finds nothing and *no* control on that vendor would ever correlate.
    ///
    /// Each vendor marks direction its own way — `codex` by `method`, `claude` by
    /// `type: "control_response"`, `pi` by `type: "response"` — so the knowledge belongs in
    /// the module that owns the vendor. Boxed rather than a generic parameter because the
    /// adapters are held as `dyn Adapter` and one concrete `Child` type is what lets the
    /// read loop be one implementation.
    pub reply_id: Option<ReplyId>,
}

/// Reads the id off a frame that is a reply, or `None` for anything else.
///
/// See [`Spawn::reply_id`] for why this is a closure. `Sync` because the read loop holds it
/// for the process's life on a task the caller does not own.
pub type ReplyId = Box<dyn Fn(&Value) -> Option<String> + Send + Sync>;

/// The id off a JSON-RPC reply: a frame carrying `id` and **no `method`**.
///
/// Offered here rather than written per-adapter because it is a property of JSON-RPC, and
/// `codex` is not the last agent that will speak it. `id` may be a string or a number and
/// both are read — the spec permits either, and a reader accepting one silently fails to
/// correlate on the other.
pub fn json_rpc_reply_id(frame: &Value) -> Option<String> {
    if frame.get("method").is_some() {
        return None;
    }
    frame.get("id").and_then(reply_id)
}

impl Child {
    /// Start the process and take over its stdout.
    ///
    /// Returns the handle and the line stream. Lines the caller correlated with
    /// [`Self::request`] are removed from the stream, because a reply is not an event.
    pub async fn spawn(spawn: Spawn) -> Result<(Self, mpsc::Receiver<Line>)> {
        let mut command = Command::new(&spawn.program);
        command
            .args(&spawn.args)
            .current_dir(&spawn.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own group, so a stop reaches the tool the vendor spawned.
            .process_group(0);
        for (key, value) in &spawn.env {
            command.env(key, value);
        }

        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to start `{}`; it is on PATH but would not run",
                spawn.program
            )
        })?;

        // Captured before anything can reap the child: `id()` answers `None` once the
        // process has been waited on, and the group has to stay addressable for its whole
        // life — including the window where the reader task is awaiting its exit.
        let pgid = child
            .id()
            .context("the vendor process exited before reporting a pid")? as i32;

        let mut stdin = child.stdin.take().context("stdin was not piped")?;
        let stdout = child.stdout.take().context("stdout was not piped")?;
        let stderr = child.stderr.take().context("stderr was not piped")?;

        let (write_tx, mut write_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            while let Some(line) = write_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
            // Dropping stdin here is what tells a vendor waiting on it that no more input
            // is coming, which is how `claude` exits when a session closes.
        });

        let tail = Arc::new(Mutex::new(String::new()));
        let stderr_tail = tail.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut held = stderr_tail.lock().await;
                held.push_str(&line);
                held.push('\n');
                // Bounded from the front: the newest lines are the ones that explain an
                // exit, and an unbounded buffer on a chatty vendor is a leak for the
                // session's whole life.
                if held.len() > STDERR_TAIL_BYTES {
                    let cut = held.len() - STDERR_TAIL_BYTES;
                    *held = held[cut..].to_string();
                }
            }
        });

        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (line_tx, line_rx) = mpsc::channel::<Line>(256);
        let reply_id_of = spawn.reply_id;
        let routed = pending.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let ended = closed.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut raw = Vec::new();
            loop {
                raw.clear();
                match reader.read_until(b'\n', &mut raw).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if raw.last() == Some(&b'\n') {
                            raw.pop();
                        }
                        if raw.iter().all(u8::is_ascii_whitespace) {
                            continue;
                        }
                        let parsed: Value = match serde_json::from_slice(&raw) {
                            Ok(value) => value,
                            Err(error) => {
                                // Reported, not fatal. A vendor may print a banner, and one
                                // malformed line must not fail a turn.
                                let _ = line_tx
                                    .send(Err(anyhow!("unparsable vendor line: {error}")))
                                    .await;
                                continue;
                            }
                        };
                        // A reply is not an event: it wakes its caller and leaves the
                        // stream, or a `set_model` acknowledgement would appear in the
                        // transcript. Everything else continues on, **including a request
                        // the vendor itself raised** — an approval routed into a waiter is
                        // a question nobody is asked (see [`Spawn::reply_id`]).
                        if let Some(read_id) = &reply_id_of {
                            if let Some(id) = read_id(&parsed) {
                                if let Some(waiter) = routed.lock().await.remove(&id) {
                                    let _ = waiter.send(parsed);
                                    continue;
                                }
                            }
                        }
                        if line_tx
                            .send(Ok(ProcessLine {
                                value: parsed,
                                raw: raw.clone(),
                            }))
                            .await
                            .is_err()
                        {
                            // The consumer is gone, which means the session was dropped.
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = line_tx.send(Err(anyhow!("reading stdout: {error}"))).await;
                        break;
                    }
                }
            }
            // Set **before** draining, so a request racing the drain sees the closure
            // rather than registering into a map nothing will read again.
            ended.store(true, Ordering::SeqCst);
            // Every caller still waiting is woken by the drop, which surfaces as a
            // "vendor closed" error rather than as a hang.
            routed.lock().await.clear();
            // Reaped here so the child never becomes a zombie, and its exit is not raced
            // with `close`.
            let _ = child.wait().await;
        });

        Ok((
            Self {
                stdin: write_tx,
                pending,
                next_id: AtomicU64::new(1),
                pgid,
                stderr: tail,
                closed,
            },
            line_rx,
        ))
    }

    /// Send one line, expecting no reply.
    pub async fn send(&self, payload: Value) -> Result<()> {
        self.stdin
            .send(payload.to_string())
            .await
            .map_err(|_| anyhow!("the vendor process is no longer accepting input"))
    }

    /// An id no other request on this process uses.
    ///
    /// **A bare integer, and the caller decides its JSON type.** `pi` echoes a string;
    /// JSON-RPC — which is `codex` — numbers its ids, and a number is what it echoes back.
    /// The correlation key is the decimal spelling either way, which is what makes one
    /// pending map serve both. Monotonic rather than random, so a transcript of one
    /// process's traffic reads in order when reproducing a correlation bug.
    pub fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send one line and await the reply carrying the same id.
    ///
    /// `build` receives the id so the caller can place it where its vendor expects, and in
    /// the type its vendor uses: `json!({"id": id.to_string(), …})` for `pi`,
    /// `json!({"id": id, …})` for JSON-RPC.
    ///
    /// **The registration happens before the write.** Registering afterwards loses a reply
    /// a fast vendor has already sent, which presents as an intermittent timeout on a
    /// correct implementation.
    pub async fn request(&self, build: impl FnOnce(u64) -> Value) -> Result<Value> {
        // Checked first: after EOF nothing drains the pending map, so registering would
        // buy a 30-second wait for an answer that cannot come ([`Self::closed`]).
        if self.has_ended() {
            return Err(self.error("the vendor process has exited").await);
        }
        let id = self.next_request_id();
        let payload = build(id);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.to_string(), tx);

        if let Err(error) = self.send(payload).await {
            self.pending.lock().await.remove(&id.to_string());
            return Err(error);
        }

        match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(self
                .error("the vendor process closed without answering")
                .await),
            Err(_) => {
                self.pending.lock().await.remove(&id.to_string());
                Err(anyhow!(
                    "the vendor did not answer within {}s",
                    REPLY_TIMEOUT.as_secs()
                ))
            }
        }
    }

    /// What the vendor has written to stderr.
    pub async fn stderr_tail(&self) -> String {
        self.stderr.lock().await.trim().to_string()
    }

    /// Whether the vendor's stdout has ended. See `Self::closed`.
    pub fn has_ended(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// An error carrying whatever the vendor said on stderr.
    ///
    /// Offered so no adapter has to remember to reach for the tail. A vendor writes its
    /// diagnostics there and nowhere else — a missing API key, an unwritable config, a
    /// panic — so an error built without it reports "the process exited" for all three,
    /// and the one message that could name the fix is the one discarded.
    ///
    /// Not `async` on the hot path by accident: it locks, so it belongs on a failure path
    /// rather than inside a loop.
    pub async fn error(&self, context: impl std::fmt::Display) -> anyhow::Error {
        let tail = self.stderr_tail().await;
        if tail.is_empty() {
            anyhow!("{context}")
        } else {
            anyhow!("{context}: {tail}")
        }
    }

    /// Ask the group to exit, then make sure it did.
    ///
    /// Idempotent, because the idle reaper, an explicit detach, and shutdown can all reach
    /// it. **Escalates unconditionally**: signalling an already-exited group is defined to
    /// be harmless, while a `close` a vendor ignores would otherwise leak a process for
    /// the server's life.
    pub async fn close(&self) {
        self.signal(libc::SIGTERM);
        tokio::time::sleep(SHUTDOWN_GRACE).await;
        self.signal(libc::SIGKILL);
    }

    /// Interrupt the group, as a terminal's Ctrl-C.
    ///
    /// Distinct from [`Self::close`]: a vendor may read it as "end this turn" rather than
    /// "exit", and conflating the two loses one of two real intentions.
    pub fn interrupt(&self) {
        self.signal(libc::SIGINT);
    }

    fn signal(&self, signal: i32) {
        // ESRCH means the group exited between the decision and the signal, which is a
        // benign race: whoever awaits the exit records the outcome. Reporting it would
        // make a stop that arrived slightly late look like a failure to stop.
        let sent = unsafe { libc::killpg(self.pgid, signal) };
        if sent != 0 {
            debug!(pgid = self.pgid, signal, "killpg reported no such group");
        }
    }
}

/// A reply id, whether the vendor spells it as a string or a number.
///
/// JSON-RPC permits either and `codex` uses integers, while `pi` echoes the string it was
/// given. A reader accepting only one silently fails to correlate on the other, and an
/// uncorrelated reply looks like a vendor that never answered.
///
/// Public because a [`Spawn::reply_id`] predicate is written per vendor, and one that finds
/// its id somewhere other than the top level — `claude`'s is nested under `response` —
/// still needs to read it once it has. Keeping this private left [`json_rpc_reply_id`]
/// exported while the read it is built from was not, so the only reusable spelling was the
/// one shape that happened to have a helper.
///
/// **Use the narrower read where a vendor's own shape makes a branch unreachable.** `pi`
/// echoes `command.id` verbatim and this side only ever sends a string, so a number is not
/// a shape that vendor produces — accepting one would be tolerance for something nothing
/// emits, and `AGENTS.md` § Compatibility prefers the loud break. This exists for the
/// vendors that genuinely span both.
pub fn reply_id(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spawn_sh(script: &str, reply_id: Option<ReplyId>) -> Spawn {
        Spawn {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            cwd: "/tmp".to_string(),
            env: HashMap::new(),
            reply_id,
        }
    }

    /// A flat top-level `id`, with no direction discriminant. What the first version of
    /// this module did for every vendor.
    fn flat_id() -> Option<ReplyId> {
        Some(Box::new(|frame: &Value| frame.get("id").and_then(reply_id)))
    }

    /// JSON-RPC's own rule, which is what `codex` needs.
    fn rpc_id() -> Option<ReplyId> {
        Some(Box::new(json_rpc_reply_id))
    }

    /// Lines arrive parsed, in order.
    #[tokio::test]
    async fn stdout_lines_arrive_parsed_and_in_order() {
        let (_child, mut lines) = Child::spawn(spawn_sh(r#"printf '{"n":1}\n{"n":2}\n'"#, None))
            .await
            .unwrap();

        assert_eq!(lines.recv().await.unwrap().unwrap()["n"], json!(1));
        assert_eq!(lines.recv().await.unwrap().unwrap()["n"], json!(2));
    }

    #[tokio::test]
    async fn stdout_lines_retain_lexical_json_bytes() {
        let (_child, mut lines) = Child::spawn(spawn_sh(
            r#"printf ' {"b":"\u0061", "a": 1}  \n'"#,
            None,
        ))
        .await
        .unwrap();

        let line = lines.recv().await.unwrap().unwrap();
        assert_eq!(line.value, json!({"b": "a", "a": 1}));
        assert_eq!(line.raw, br#" {"b":"\u0061", "a": 1}  "#);
    }

    /// **An unparsable line is reported and the stream continues.**
    ///
    /// A vendor that prints a banner, or one malformed line mid-turn, must not fail the
    /// turn — and it must not be silently dropped either, or the record cannot explain a
    /// gap.
    #[tokio::test]
    async fn an_unparsable_line_is_reported_without_ending_the_stream() {
        let (_child, mut lines) = Child::spawn(spawn_sh(r#"printf 'not json\n{"n":1}\n'"#, None))
            .await
            .unwrap();

        assert!(lines.recv().await.unwrap().is_err());
        assert_eq!(lines.recv().await.unwrap().unwrap()["n"], json!(1));
    }

    /// A blank line is neither an event nor an error.
    #[tokio::test]
    async fn blank_lines_are_skipped_entirely() {
        let (_child, mut lines) = Child::spawn(spawn_sh(r#"printf '\n\n{"n":1}\n'"#, None))
            .await
            .unwrap();

        assert_eq!(lines.recv().await.unwrap().unwrap()["n"], json!(1));
    }

    /// A reply wakes its caller and **does not reach the stream**.
    ///
    /// Otherwise a `set_model` acknowledgement would appear in the transcript as an event.
    #[tokio::test]
    async fn a_correlated_reply_leaves_the_stream() {
        let script = r#"read -r line; printf '{"id":"1","ok":true}\n{"n":1}\n'"#;
        let (child, mut lines) = Child::spawn(spawn_sh(script, flat_id())).await.unwrap();

        let reply = child
            .request(|id| json!({"id": id.to_string(), "method": "ping"}))
            .await
            .unwrap();
        assert_eq!(reply["ok"], json!(true));
        // The next line off the stream is the event, not the reply.
        assert_eq!(lines.recv().await.unwrap().unwrap()["n"], json!(1));
    }

    /// **A numeric reply id correlates**, because JSON-RPC numbers its ids and `codex` is
    /// JSON-RPC.
    ///
    /// A reader accepting only strings fails to correlate on a number, and an uncorrelated
    /// reply is indistinguishable from a vendor that never answered — while also leaking
    /// into the event stream as an event nothing emitted.
    #[tokio::test]
    async fn a_numeric_reply_id_correlates() {
        // Echoes back the id it was sent, as a JSON *number*, exactly as a JSON-RPC peer
        // does.
        let script = r#"read -r line; id=$(printf '%s' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/'); printf '{"id":%s,"ok":true}\n' "$id""#;
        let (child, _lines) = Child::spawn(spawn_sh(script, flat_id())).await.unwrap();

        let reply = child
            .request(|id| json!({"id": id, "method": "ping"}))
            .await
            .expect("a numeric echo must correlate");
        assert_eq!(reply["ok"], json!(true));
    }

    /// A string reply id correlates, which is what `pi` echoes.
    #[tokio::test]
    async fn a_string_reply_id_correlates() {
        let script = r#"read -r line; id=$(printf '%s' "$line" | sed 's/.*"id":"\([^"]*\)".*/\1/'); printf '{"id":"%s","ok":true}\n' "$id""#;
        let (child, _lines) = Child::spawn(spawn_sh(script, flat_id())).await.unwrap();

        let reply = child
            .request(|id| json!({"id": id.to_string(), "method": "ping"}))
            .await
            .expect("a string echo must correlate");
        assert_eq!(reply["ok"], json!(true));
    }

    /// A vendor that exits without answering fails the request rather than hanging.
    #[tokio::test]
    async fn a_vendor_that_exits_without_answering_fails_the_request() {
        let (child, _lines) = Child::spawn(spawn_sh("exit 0", flat_id())).await.unwrap();

        let error = child
            .request(|id| json!({"id": id}))
            .await
            .expect_err("a closed process cannot answer");
        assert!(
            error.to_string().contains("closed") || error.to_string().contains("accepting"),
            "the error must name the closure, got: {error}"
        );
    }

    /// stderr is kept, so a failed launch can say why.
    #[tokio::test]
    async fn stderr_is_retained_for_an_error_to_quote() {
        let (child, mut lines) = Child::spawn(spawn_sh("echo boom >&2; exit 1", None))
            .await
            .unwrap();

        // Drain to EOF so the stderr task has run.
        while lines.recv().await.is_some() {}
        for _ in 0..50 {
            if child.stderr_tail().await.contains("boom") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("stderr was not retained: {:?}", child.stderr_tail().await);
    }

    /// A program that is not there fails at spawn, naming it.
    #[tokio::test]
    async fn a_missing_program_fails_naming_itself() {
        let outcome = Child::spawn(Spawn {
            program: "sylph-no-such-binary".to_string(),
            args: vec![],
            cwd: "/tmp".to_string(),
            env: HashMap::new(),
            reply_id: None,
        })
        .await;
        let error = match outcome {
            Ok(_) => panic!("a missing program cannot spawn"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("sylph-no-such-binary"));
    }

    /// **`close` reaches the vendor's children, not only the vendor.**
    ///
    /// The child here outlives its parent shell, so signalling the parent alone leaves it
    /// running — which for a real agent means a tool call that keeps going after the
    /// session is gone.
    #[tokio::test]
    async fn close_signals_the_whole_process_group() {
        let script = "sleep 30 & printf '{\"pid\":'$!'}\\n'; wait";
        let (child, mut lines) = Child::spawn(spawn_sh(script, None)).await.unwrap();

        let grandchild = lines.recv().await.unwrap().unwrap()["pid"]
            .as_i64()
            .expect("the script reports its child's pid") as i32;
        assert_eq!(
            unsafe { libc::kill(grandchild, 0) },
            0,
            "the grandchild must be alive before close"
        );

        child.close().await;

        for _ in 0..50 {
            if unsafe { libc::kill(grandchild, 0) } != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        panic!("the grandchild survived close, so the group was not signalled");
    }

    /// **A request the vendor raised must not be routed into a waiter.**
    ///
    /// *Measured on `codex` in `docs/measurements/codex/captures/s15_approval_decisions.jsonl`:*
    /// its server→client approvals are numbered `0, 1, 2` in the **same** space as our own
    /// requests, which hold `1..5`. So while one of ours is outstanding, an approval
    /// carrying that number is indistinguishable by id alone.
    ///
    /// Both directions of the failure are silent: the approval never reaches the event
    /// stream, so nobody is asked and the turn waits on a human forever, while the caller
    /// receives an approval request as its reply. JSON-RPC's discriminant is `method`.
    #[tokio::test]
    async fn a_vendor_initiated_request_is_not_mistaken_for_a_reply() {
        // Emits an approval numbered 1 — colliding with our first request id — and only
        // then the real reply.
        let script = r#"read -r line; printf '{"id":1,"method":"item/commandExecution/requestApproval","params":{}}\n{"id":1,"result":{"ok":true}}\n'"#;
        let (child, mut lines) = Child::spawn(spawn_sh(script, rpc_id())).await.unwrap();

        let reply = child
            .request(|id| json!({"id": id, "method": "thread/settings/update"}))
            .await
            .expect("the reply must reach the caller, not the approval");
        assert_eq!(
            reply["result"]["ok"],
            json!(true),
            "a `method`-bearing frame is a request and must never satisfy a waiter"
        );

        // And the approval reached the stream, where a client can answer it.
        let event = lines.recv().await.unwrap().unwrap();
        assert_eq!(
            event["method"],
            json!("item/commandExecution/requestApproval"),
            "the vendor's request must be published, or nobody is asked"
        );
    }

    /// The same collision, against the design that could not express the difference.
    ///
    /// Kept as a test rather than only as prose: it is the evidence that a bare field name
    /// is not merely less general but *wrong* for a vendor that initiates requests, and it
    /// fails loudly if anyone reintroduces one.
    #[tokio::test]
    async fn correlating_on_the_id_alone_swallows_a_vendor_request() {
        let script = r#"read -r line; printf '{"id":1,"method":"item/commandExecution/requestApproval","params":{}}\n{"id":1,"result":{"ok":true}}\n'"#;
        let (child, _lines) = Child::spawn(spawn_sh(script, flat_id())).await.unwrap();

        let reply = child
            .request(|id| json!({"id": id, "method": "thread/settings/update"}))
            .await
            .unwrap();
        // The approval satisfied the waiter. Nothing errored anywhere.
        assert_eq!(
            reply["method"],
            json!("item/commandExecution/requestApproval"),
            "this is the defect: a request arrives as a reply and no layer reports it"
        );
    }

    /// A reply whose id is nested rather than top-level still correlates.
    ///
    /// `claude` spells one `{"type":"control_response","response":{"request_id":…}}`, so a
    /// top-level lookup finds nothing and **no** control on that vendor would correlate at
    /// all — a second thing a bare field name cannot express.
    #[tokio::test]
    async fn a_nested_reply_id_correlates() {
        let script = r#"read -r line; printf '{"type":"control_response","response":{"request_id":"1","subtype":"success"}}\n'"#;
        let nested: Option<ReplyId> = Some(Box::new(|frame: &Value| {
            if frame.get("type").and_then(Value::as_str) != Some("control_response") {
                return None;
            }
            frame
                .get("response")
                .and_then(|body| body.get("request_id"))
                .and_then(reply_id)
        }));
        let (child, _lines) = Child::spawn(spawn_sh(script, nested)).await.unwrap();

        let reply = child
            .request(|id| json!({"type": "control_request", "request_id": id.to_string()}))
            .await
            .expect("a nested id must correlate");
        assert_eq!(reply["response"]["subtype"], json!("success"));
    }

    /// **A request after the vendor has exited fails at once, not after the timeout.**
    ///
    /// The reader task is what drains the pending map on EOF, and it has already gone — so
    /// a request registered afterwards would wait the full 30-second budget and then report
    /// that the vendor "did not answer", which is the wrong sentence for a process that is
    /// not running. Measured before the fix: the whole budget.
    ///
    /// A write cannot reveal this, which is why the flag exists rather than a check on the
    /// send: the writer task is alive and the OS buffers a write to a pipe whose reader is
    /// gone, so the write succeeds.
    #[tokio::test]
    async fn a_request_after_the_vendor_exits_fails_at_once() {
        let (child, mut lines) = Child::spawn(spawn_sh("exit 0", flat_id())).await.unwrap();
        while lines.recv().await.is_some() {}

        let started = std::time::Instant::now();
        let error = match child.request(|id| json!({"id": id})).await {
            Ok(_) => panic!("an exited vendor cannot answer"),
            Err(error) => error,
        };
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "took {:?}; a dead vendor must not cost the reply timeout",
            started.elapsed()
        );
        assert!(
            error.to_string().contains("exited"),
            "the error must name the exit rather than a timeout, got: {error}"
        );
    }

    /// An error built by [`Child::error`] carries what the vendor said.
    ///
    /// A vendor writes its diagnostics to stderr and nowhere else, so an error built without
    /// the tail reports "the process exited" for a missing credential, an unwritable config,
    /// and a panic alike — discarding the one message that names the fix.
    #[tokio::test]
    async fn an_error_quotes_what_the_vendor_said_on_stderr() {
        let (child, mut lines) = Child::spawn(spawn_sh("echo 'no API key' >&2; exit 1", flat_id()))
            .await
            .unwrap();
        while lines.recv().await.is_some() {}

        for _ in 0..50 {
            let error = child.error("could not start a turn").await.to_string();
            if error.contains("no API key") {
                assert!(error.contains("could not start a turn"));
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the vendor's own words never reached the error");
    }

    /// Ids are unique for the process's life.
    #[tokio::test]
    async fn request_ids_do_not_repeat() {
        let (child, _lines) = Child::spawn(spawn_sh("sleep 1", flat_id())).await.unwrap();
        let ids: Vec<u64> = (0..5).map(|_| child.next_request_id()).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
    }
}
