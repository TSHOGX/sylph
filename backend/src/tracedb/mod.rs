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
    pub async fn probe(&self) -> Option<String> {
        let stats = self.stats().await.ok()?;
        stats.get("version").and_then(|v| v.as_str()).map(str::to_string)
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

    /// Show: metadata and the normalized event stream for one session.
    pub async fn show(&self, id: &str) -> Result<serde_json::Value, String> {
        self.call("show", None, None, Some(id)).await
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
