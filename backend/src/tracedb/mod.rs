//! TraceDB client: stats, search, show via the JSON line protocol.
//!
//! The backend spawns `trace-db api` at startup and keeps its stdin/stdout
//! open for request/response pairs. Protocol: send one JSON line per request,
//! read one JSON line per response. See
//! `/Users/hsw/Workspace/trace-db/references/protocol.md`.

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

#[derive(Serialize)]
struct Request<'a> {
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a str>,
}

#[derive(Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    result: serde_json::Value,
}

/// Archive-wide totals. Each count is optional because the protocol's fields
/// are additive and a future server may omit or rename one; a missing count
/// must not turn a reachable archive into an unreachable one.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveCounts {
    pub sessions: Option<i64>,
    pub events: Option<i64>,
}

/// One normalized event from an archived session. Fields are all optional
/// because the protocol's fields are additive and older sessions predate some
/// of them.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEvent {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub created_at_ms: Option<i64>,
}

/// Facts about an archived session, derived from its own event stream.
#[derive(Debug, Clone)]
pub struct SessionFacts {
    /// Working directory recorded by the agent, when it recorded one.
    pub cwd: Option<String>,
    /// The opening user turn, used as a fallback title.
    pub first_ask: Option<String>,
    pub started_at_ms: Option<i64>,
    pub event_count: usize,
}

/// The snippet TraceDB judged most relevant within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BestMatch {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub snippet: Option<String>,
}

/// One lineage-collapsed search hit.
///
/// Deliberately a subset of what TraceDB returns: scoring internals
/// (`score`, `scoreBreakdown`, `bm25`) are ranking mechanics with no meaning in
/// the UI, and passing them through would couple this API to TraceDB's scoring
/// implementation. Unknown fields are ignored, per the protocol's rule that new
/// fields are additive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// TraceDB session id, e.g. `claude:uuid/agent-suffix`.
    pub id: String,
    pub agent: String,
    /// Human-authored title when the agent recorded one; often absent.
    #[serde(default)]
    pub title: Option<String>,
    /// The opening user request, which is what makes a hit recognizable.
    #[serde(default)]
    pub ask: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub best_match: Option<BestMatch>,
    #[serde(default)]
    pub hits: Option<u32>,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    #[serde(default)]
    pub ended_at_ms: Option<i64>,
    /// Root of the fork lineage this session belongs to.
    #[serde(default)]
    pub lineage_root_id: Option<String>,
}

impl Client {
    pub fn new(bin_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            bin_path,
        }
    }

    /// Probe TraceDB availability: try spawning the process and reading its
    /// version via `stats`, returning `Some(version)` on success.
    ///
    /// A failed spawn or a failed stats request leaves the connection absent,
    /// so the next operation will try again. This implements lazy startup: if
    /// TraceDB is misconfigured, the backend still boots and serves sessions.
    /// Whether the archive answers a `stats` request right now.
    ///
    /// Reachability is the successful round trip itself, not the presence of
    /// any particular field. An earlier version keyed this on a `version` field
    /// in the stats payload, which the protocol does not define — so a
    /// perfectly healthy archive always reported unavailable, and the health
    /// endpoint contradicted a search that was working.
    pub async fn probe(&self) -> bool {
        self.stats().await.is_ok()
    }

    /// Archive-wide counts for the authenticated diagnostics endpoint.
    ///
    /// Returns `None` when the archive is unreachable, which is the same
    /// meaning [`Self::probe`] carries.
    pub async fn archive_counts(&self) -> Option<ArchiveCounts> {
        let stats = self.stats().await.ok()?;
        Some(ArchiveCounts {
            sessions: stats.get("totalSessions").and_then(|v| v.as_i64()),
            events: stats.get("totalEvents").and_then(|v| v.as_i64()),
        })
    }

    /// Stats: archive health and per-agent session counts.
    pub async fn stats(&self) -> Result<serde_json::Value, String> {
        self.call("stats", None, None, None).await
    }

    /// Search: lineage-collapsed session hits, best match first.
    ///
    /// `result` is a bare JSON array of hits (not an object), matching
    /// `trace-db api`'s `{"op":"search"}` response.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
        let raw = self.call("search", Some(query), Some(limit), None).await?;
        serde_json::from_value(raw).map_err(|e| format!("Unexpected search payload: {}", e))
    }

    /// Show: the normalized event stream for one session.
    ///
    /// `result` is a bare array of events. Despite the protocol table's
    /// wording, it carries no session metadata object — no agent, title, cwd,
    /// or outcome. Those appear only on a search hit, so anything the import
    /// path needs about the session itself has to be derived from the events
    /// or from the id.
    pub async fn show(&self, id: &str) -> Result<Vec<TraceEvent>, String> {
        let raw = self.call("show", None, None, Some(id)).await?;
        serde_json::from_value(raw).map_err(|e| format!("Unexpected show payload: {}", e))
    }

    /// What the import path needs to know about an archived session, derived
    /// from its event stream rather than taken from the client.
    ///
    /// Returns `Ok(None)` when the archive has no such session, which is
    /// distinct from the archive being unreachable (`Err`).
    pub async fn session_facts(&self, id: &str) -> Result<Option<SessionFacts>, String> {
        let events = self.show(id).await?;
        if events.is_empty() {
            return Ok(None);
        }

        // The agent runs a `system` event early carrying the session's working
        // directory. Taking it from the archive keeps a policy-relevant field
        // authoritative rather than trusting whatever a client posts.
        let cwd = events.iter().find_map(|event| {
            let text = event.text.as_deref()?;
            let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
            parsed
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });

        // The opening user turn is the session's recognizable summary; `show`
        // exposes no title.
        let first_ask = events
            .iter()
            .find(|event| event.kind.as_deref() == Some("user"))
            .and_then(|event| event.text.clone());

        let started_at_ms = events
            .iter()
            .filter_map(|event| event.created_at_ms)
            .find(|ms| *ms > 0);

        Ok(Some(SessionFacts {
            cwd,
            first_ask,
            started_at_ms,
            event_count: events.len(),
        }))
    }

    /// Unified call path: spawn the process if absent, serialize the request,
    /// read the response, and return the result.
    async fn call(
        &self,
        op: &str,
        query: Option<&str>,
        limit: Option<usize>,
        id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
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
        let req = Request {
            op,
            query,
            limit,
            id,
        };

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

    /// Spawn the TraceDB API process: `trace-db api --db ~/.../trace.db`.
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
