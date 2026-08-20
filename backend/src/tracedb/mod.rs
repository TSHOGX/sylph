//! TraceDB client: stats, search, show via the JSON line protocol.
//!
//! The backend spawns `trace-db api` at startup and keeps its stdin/stdout
//! open for request/response pairs. Protocol: send one JSON line per request,
//! read one JSON line per response. See
//! `/Users/hsw/Workspace/trace-db/references/protocol.md`.
//!
//! This targets the current TraceDB build and nothing older. Verified against
//! `trace-db` at `e84cb7c` on 2026-08-20 by driving `trace-db api` directly and
//! reading its responses, so the field spellings below are observed rather than
//! inferred:
//!
//! - `stats` and `search` serialize camelCase (`totalSessions`, `startedAtMs`).
//! - `show` serializes snake_case (`created_at_ms`, `started_at_ms`), because
//!   its payload is built from `model.rs` types that carry no `rename_all`.
//!
//! The inconsistency is TraceDB's, not ours, and each spelling is pinned to the
//! operation that actually emits it. Do not add cross-spelling aliases: an
//! alias would let a future rename pass silently as `None` instead of failing a
//! test, which is exactly how the archive-metadata path went dead before.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

/// A long-lived TraceDB API process. All methods are async-safe and can be
/// called concurrently: interior locking serializes requests.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Mutex<Option<Connection>>>,
    bin_path: PathBuf,
}

struct Connection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// One request in the line protocol. Fields are per-operation and omitted when
/// absent, since TraceDB rejects an unparsable value but ignores a missing one.
#[derive(Default, Serialize)]
struct Request<'a> {
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a str>,
    /// Inclusive event index bounds, used to fetch metadata without the whole
    /// event stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<i64>,
    /// Event kinds to keep. Applied before `from`/`to`, so the two cannot be
    /// combined to fetch "the first event of this kind".
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    kind: &'a [&'a str],
}

#[derive(Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    result: serde_json::Value,
}

/// Archive-wide totals for the authenticated diagnostics endpoint.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveCounts {
    pub sessions: Option<i64>,
    pub events: Option<i64>,
}

/// A `show` response: session metadata plus the requested slice of events.
///
/// `null` is a clean "no such session" rather than a protocol error, so it
/// deserializes to `None` instead of failing.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionTrace {
    pub session: TraceSession,
    pub events: Vec<TraceEvent>,
}

/// Session-level metadata from `show`.
///
/// snake_case is `show`'s actual spelling. `cwd`, `title`, and `started_at_ms`
/// are genuinely absent for many archived sessions — measured over a 400-session
/// sample of the local archive: 19% have no `cwd` (almost all `pi`), and
/// essentially none carry a `title` or `started_at_ms`. So these are `Option`
/// because the data is sparse, not because the protocol is uncertain.
#[derive(Debug, Clone, Deserialize)]
pub struct TraceSession {
    pub id: String,
    pub agent: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
}

/// One normalized event from an archived session.
#[derive(Debug, Clone, Deserialize)]
pub struct TraceEvent {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub created_at_ms: Option<i64>,
}

/// Facts the import path needs about an archived session.
#[derive(Debug, Clone)]
pub struct SessionFacts {
    /// Working directory recorded by the archive. `None` means unresumable.
    pub cwd: Option<String>,
    /// A recognizable summary: the archive's title, else the opening user turn.
    pub first_ask: Option<String>,
    pub started_at_ms: Option<i64>,
}

/// The snippet TraceDB judged most relevant within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BestMatch {
    pub kind: String,
    pub snippet: String,
}

/// One lineage-collapsed search hit.
///
/// Deliberately a subset of what TraceDB returns: scoring internals (`score`,
/// `scoreBreakdown`, and `bestMatch.bm25`) are ranking mechanics with no meaning
/// in the UI, and passing them through would couple this API to TraceDB's
/// scoring implementation. Unknown fields are ignored, per the protocol's rule
/// that new fields are additive.
///
/// The `Option` fields are the ones TraceDB itself declares optional; `id`,
/// `agent`, `hits`, and `bestMatch` are always present.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// TraceDB session id, e.g. `claude:uuid/agent-suffix`.
    pub id: String,
    pub agent: String,
    /// Human-authored title when the agent recorded one; usually absent.
    #[serde(default)]
    pub title: Option<String>,
    /// The opening user request, which is what makes a hit recognizable.
    #[serde(default)]
    pub ask: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    pub best_match: BestMatch,
    /// How many events inside the session matched.
    pub hits: i64,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    #[serde(default)]
    pub ended_at_ms: Option<i64>,
    /// Root of the fork lineage this session belongs to.
    pub lineage_root_id: String,
}

impl Client {
    pub fn new(bin_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            bin_path,
        }
    }

    /// Whether the archive answers a `stats` request right now.
    ///
    /// Reachability is the successful round trip itself, not the presence of any
    /// particular field. An earlier version keyed this on a `version` field the
    /// protocol does not define, so a healthy archive always reported
    /// unavailable and the health endpoint contradicted a working search.
    ///
    /// A failed spawn leaves the connection absent, so the next call retries.
    /// That is deliberate: the backend boots and serves live sessions even when
    /// TraceDB is missing.
    pub async fn probe(&self) -> bool {
        self.stats().await.is_ok()
    }

    /// Archive-wide counts. `None` when the archive is unreachable, which is the
    /// same meaning [`Self::probe`] carries.
    pub async fn archive_counts(&self) -> Option<ArchiveCounts> {
        let stats = self.stats().await.ok()?;
        Some(ArchiveCounts {
            sessions: stats.get("totalSessions").and_then(|v| v.as_i64()),
            events: stats.get("totalEvents").and_then(|v| v.as_i64()),
        })
    }

    /// Stats: archive health and per-agent session counts.
    pub async fn stats(&self) -> Result<serde_json::Value, String> {
        self.call(Request {
            op: "stats",
            ..Default::default()
        })
        .await
    }

    /// Search: lineage-collapsed session hits, best match first. `result` is a
    /// bare JSON array of hits.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
        let raw = self
            .call(Request {
                op: "search",
                query: Some(query),
                limit: Some(limit),
                ..Default::default()
            })
            .await?;
        serde_json::from_value(raw).map_err(|e| format!("Unexpected search payload: {}", e))
    }

    /// Show: session metadata plus events, optionally narrowed to an inclusive
    /// event index range. `Ok(None)` means the archive has no such session,
    /// which is distinct from the archive being unreachable (`Err`).
    pub async fn show(
        &self,
        id: &str,
        range: Option<(i64, i64)>,
    ) -> Result<Option<SessionTrace>, String> {
        let (from, to) = match range {
            Some((from, to)) => (Some(from), Some(to)),
            None => (None, None),
        };
        let raw = self
            .call(Request {
                op: "show",
                id: Some(id),
                from,
                to,
                ..Default::default()
            })
            .await?;
        serde_json::from_value(raw).map_err(|e| format!("Unexpected show payload: {}", e))
    }

    /// What the import path needs about an archived session, taken from the
    /// archive rather than from whatever a client posts.
    ///
    /// Two requests instead of one: the first reads metadata with a one-event
    /// slice, and the second fetches user events only when the metadata carries
    /// no title. Sessions here routinely hold thousands of events and the
    /// largest exceed a megabyte of JSON, so pulling a whole stream to read two
    /// fields off its header would dominate the cost of an import.
    pub async fn session_facts(&self, id: &str) -> Result<Option<SessionFacts>, String> {
        let Some(trace) = self.show(id, Some((0, 0))).await? else {
            return Ok(None);
        };

        let started_at_ms = trace
            .session
            .started_at_ms
            .filter(|ms| *ms > 0)
            .or_else(|| {
                trace
                    .events
                    .iter()
                    .filter_map(|event| event.created_at_ms)
                    .find(|ms| *ms > 0)
            });

        // The archive's own title when it has one. Almost no session carries
        // one, so the opening user turn is the usual path, not the exception.
        let first_ask = match trace.session.title {
            Some(title) => Some(title),
            None => self.first_user_text(id).await?,
        };

        Ok(Some(SessionFacts {
            cwd: trace.session.cwd,
            first_ask,
            started_at_ms,
        }))
    }

    /// Text of the session's opening user turn.
    ///
    /// `kind` filters before `from`/`to` apply, so the two cannot be combined to
    /// request "the first user event" — `{"kind":["user"],"to":0}` returns
    /// nothing, because index 0 is almost never a user event. So this asks for
    /// user events and takes the first: a dozen rows on a session whose full
    /// stream runs to thousands.
    async fn first_user_text(&self, id: &str) -> Result<Option<String>, String> {
        let raw = self
            .call(Request {
                op: "show",
                id: Some(id),
                kind: &["user"],
                ..Default::default()
            })
            .await?;

        // Deserializing only what is needed keeps a 1800-event stream from
        // becoming 1800 fully-materialized structs.
        #[derive(Deserialize)]
        struct UserEvents {
            events: Vec<TraceEvent>,
        }

        let trace: Option<UserEvents> = serde_json::from_value(raw)
            .map_err(|e| format!("Unexpected show payload: {}", e))?;

        Ok(trace.and_then(|trace| {
            trace
                .events
                .into_iter()
                .find(|event| event.kind.as_deref() == Some("user"))
                .and_then(|event| event.text)
        }))
    }

    /// Unified call path: spawn the process if absent, serialize the request,
    /// read the response, and return the result.
    async fn call(&self, req: Request<'_>) -> Result<serde_json::Value, String> {
        let mut guard = self.inner.lock().await;

        // Spawn if absent or if the child has exited.
        if guard
            .as_mut()
            .and_then(|conn| conn.child.try_wait().transpose())
            .is_none()
        {
            *guard = self.spawn().await?;
        }

        let conn = guard.as_mut().unwrap();

        let req_json = serde_json::to_string(&req).map_err(|e| {
            error!("Failed to serialize TraceDB request: {}", e);
            format!("JSON encoding error: {}", e)
        })?;

        conn.stdin
            .write_all(req_json.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to TraceDB stdin: {}", e))?;
        conn.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("Failed to write newline: {}", e))?;
        conn.stdin
            .flush()
            .await
            .map_err(|e| format!("Failed to flush stdin: {}", e))?;

        let mut line = String::new();
        conn.stdout
            .read_line(&mut line)
            .await
            .map_err(|e| format!("Failed to read from TraceDB stdout: {}", e))?;

        let response: Response = serde_json::from_str(&line).map_err(|e| {
            warn!("Malformed TraceDB response: {}", line.trim());
            format!("JSON decoding error: {}", e)
        })?;

        if response.ok {
            Ok(response.result)
        } else {
            let error = response
                .result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            Err(format!("TraceDB error: {}", error))
        }
    }

    /// Spawn the TraceDB API process.
    ///
    /// No `--db` argument: TraceDB resolves its own database through its config
    /// layering (`TRACEDB_PATH`, then its config file, then the platform data
    /// directory). Passing a path here would let this backend disagree with
    /// every other TraceDB client on the machine about which archive is the
    /// archive.
    async fn spawn(&self) -> Result<Option<Connection>, String> {
        debug!("Spawning TraceDB API process: {:?}", self.bin_path);

        let mut child = Command::new(&self.bin_path)
            .arg("api")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                error!("Failed to spawn TraceDB at {:?}: {}", self.bin_path, e);
                format!("spawn failed: {}", e)
            })?;

        let stdin = child.stdin.take().ok_or("stdin not captured")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("stdout not captured")?);

        Ok(Some(Connection {
            child,
            stdin,
            stdout,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Payloads here are copied from live `trace-db api` output, so a spelling
    /// change in TraceDB fails a test instead of silently yielding `None`.

    #[test]
    fn show_parses_the_snake_case_payload_trace_db_emits() {
        let trace: Option<SessionTrace> = serde_json::from_str(
            r#"{"session":{"id":"claude:x","agent":"claude","cwd":"/w/app",
                "title":"Fix login","started_at_ms":1787181939317,"git_branch":"main"},
                "mode":"full",
                "events":[{"idx":0,"kind":"user","text":"hi","created_at_ms":7}]}"#,
        )
        .expect("show payload should deserialize");

        let trace = trace.expect("a session object is not a missing session");
        assert_eq!(trace.session.cwd.as_deref(), Some("/w/app"));
        assert_eq!(trace.session.title.as_deref(), Some("Fix login"));
        assert_eq!(trace.session.started_at_ms, Some(1787181939317));
        assert_eq!(trace.events[0].created_at_ms, Some(7));
    }

    #[test]
    fn show_reports_an_unknown_session_as_null() {
        let trace: Option<SessionTrace> =
            serde_json::from_str("null").expect("null is a valid show result");

        assert!(trace.is_none(), "null means no such session");
    }

    #[test]
    fn show_tolerates_a_session_with_no_recorded_metadata() {
        // 19% of the local archive has no cwd, and nearly none has a title or
        // start time. Sparse metadata is the normal case, not a malformed one.
        let trace: Option<SessionTrace> = serde_json::from_str(
            r#"{"session":{"id":"pi:x","agent":"pi","cwd":null,"title":null,
                "started_at_ms":null},"mode":"metadata","events":[]}"#,
        )
        .expect("sparse metadata should deserialize");

        let session = trace.expect("present").session;
        assert_eq!(session.agent, "pi");
        assert!(session.cwd.is_none() && session.title.is_none());
    }

    #[test]
    fn show_ignores_added_session_fields() {
        let trace: Option<SessionTrace> = serde_json::from_str(
            r#"{"session":{"id":"x","agent":"claude","cwd":"/w","future_field":1},
                "events":[]}"#,
        )
        .expect("an added archive field must not break parsing");

        assert_eq!(trace.unwrap().session.cwd.as_deref(), Some("/w"));
    }

    #[test]
    fn search_parses_the_camel_case_payload_trace_db_emits() {
        let hits: Vec<SearchHit> = serde_json::from_str(
            r#"[{"id":"claude:x","agent":"claude","cwd":"/w","title":null,
                "ask":"fix it","outcome":"done","hits":70,
                "startedAtMs":1787181939317,"endedAtMs":1787194550318,
                "lineageRootId":"claude:root",
                "bestMatch":{"eventIdx":1573,"kind":"tool_call","bm25":-10.19,
                             "snippet":"the match"},
                "score":2.12,"scoreBreakdown":{"recency":0.99},
                "relatedSessionIds":["claude:x/agent-1"]}]"#,
        )
        .expect("search payload should deserialize");

        let hit = &hits[0];
        assert_eq!(hit.started_at_ms, Some(1787181939317));
        assert_eq!(hit.ended_at_ms, Some(1787194550318));
        assert_eq!(hit.lineage_root_id, "claude:root");
        assert_eq!(hit.hits, 70);
        assert_eq!(hit.best_match.kind, "tool_call");
        assert_eq!(hit.best_match.snippet, "the match");
    }

    #[test]
    fn search_hits_serialize_as_camel_case() {
        // Our own wire contract to the frontend, asserted independently of how
        // we parse TraceDB's.
        let hits: Vec<SearchHit> = serde_json::from_str(
            r#"[{"id":"claude:x","agent":"claude","hits":1,
                "lineageRootId":"claude:x","startedAtMs":1,
                "bestMatch":{"kind":"user","snippet":"s"}}]"#,
        )
        .expect("hit should deserialize");
        let out = serde_json::to_string(&hits[0]).expect("hit should serialize");

        assert!(out.contains("\"startedAtMs\":1"), "got {out}");
        assert!(out.contains("\"lineageRootId\""), "got {out}");
        assert!(!out.contains("started_at_ms"), "got {out}");
        // Scoring internals stay out of our API surface.
        assert!(!out.contains("score"), "got {out}");
    }
}
