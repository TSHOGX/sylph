//! Workspace roll-up for the left sidebar and the settings panel.
//!
//! The frontend groups sessions by working directory to form "spaces". It could
//! do that arithmetic itself from `GET /sessions`, but two things are only
//! knowable here: the absolute path of the Quick Start directory, and counts that
//! span more sessions than a `LIMIT`-capped listing returns. The browser cannot
//! expand `~`, and the directory is configurable — so a client that derived it
//! would be wrong for anyone who set `paths.quick_start_dir`, and for every
//! deployment where `HOME` differs from the user Next.js runs as.

use axum::{
    extract::{Extension, State},
    Json,
};
use serde::Serialize;

use crate::{db::SessionFilter, error::Result, middleware::auth::SubjectId, AppState};

/// One working directory that has at least one session, or is the Quick Start
/// directory. `path` is absolute: the sidebar shortens it for display, but
/// every API call that follows needs the real path.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    /// Opaque environment id owning this path. Workspace identity is always the
    /// pair `(environment, path)`; a path without its environment is ambiguous.
    pub environment: String,
    /// Display name of the owning environment, resolved from its current snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_name: Option<String>,
    /// Home directory reported by the owning envagent. This is deliberately per row:
    /// a remote environment's home must never be inferred from server configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    pub path: String,
    /// Directory leaf, precomputed so the client does not re-derive it per row.
    pub name: String,
    pub session_count: i64,
    pub running_count: i64,
    /// Newest `updated_at_ms` across the workspace's sessions, for ordering.
    pub last_used_ms: i64,
    /// True for the Quick Start directory, which the sidebar lists separately
    /// and which is never offered for removal.
    pub is_quick_start: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacesResponse {
    /// Absolute path of the Quick Start directory, resolved server-side.
    pub quick_start_dir: String,
    /// Where the journal and attachments live. Shown read-only in settings.
    pub data_dir: String,
    pub home: String,
    /// Newest first, Quick Start included so settings can show its counts.
    pub workspaces: Vec<Workspace>,
}

/// Explicit environment-level workspace key used for Quick Start rows.
///
/// Quick Start has no user-selected directory on a remote environment. Encoding the
/// sentinel as a path keeps the workspace key total (`environment`, `path`) while
/// making it impossible to mistake the server's configured absolute path for a path
/// on another machine. The envagent resolves the actual launch directory.
// Re-export the wire constant from the protocol crate so API clients and envagents
// cannot drift into different spellings.
pub use sylph_protocol::QUICK_START_SENTINEL;

/// Aggregate over every authorized session. Counts are part of the workspace
/// contract, so a finite page would make a busy directory report a plausible
/// but wrong total.
// SQLite's `LIMIT -1` means no limit. Workspace counts are an aggregate over
// the complete authorized journal; any finite cap would undercount a busy
// workspace even when authorization is applied in SQL.
const AGGREGATE_LIMIT: i64 = -1;

/// GET /api/v1/workspaces
///
/// # `includeArchived` is gone, and its absence is the point
///
/// The parameter existed because a directory whose only history sat in the *separate
/// archive* was invisible to the journal — so without an opt-in archive scan a user
/// could not pin it, pick it, or discover it had any history at all. With one store
/// (`docs/architecture.md` § One authority) native history is parsed into the same
/// journal, so **every directory with history is in the listing already** and the
/// distinction has nothing left to draw.
///
/// That also deletes the paged bound the scan needed (`ARCHIVE_MAX_PAGES`), and with it
/// the starvation hazard `AGENTS.md` records: because the archive's `cwd` filter was an
/// unanchored substring, a parent directory's pattern matched everything beneath it,
/// those sorted by recency rather than depth, and a directory with 88 sessions of its own
/// reported empty because its first exact match landed ~1300 rows in.
pub async fn list(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
) -> Result<Json<WorkspacesResponse>> {
    // The server owns no remote filesystem. In particular, it must not canonicalize
    // or compare a remote path against its own configuration. Session workdirs were
    // already canonicalized by the envagent that owns them; here we only normalize
    // trailing separators for stable grouping.
    // This field is a selector, not a filesystem path. The envagent resolves it to its
    // own environment-local scratch directory when a session is created.
    let quick_start_dir = QUICK_START_SENTINEL.to_string();
    let environments = state
        .env_bridge
        .list_environments()
        .await
        .map_err(|error| crate::error::Error::Internal(format!("{error:#}")))?;
    let environment_meta: std::collections::HashMap<String, (String, Option<String>)> =
        environments
            .iter()
            .map(|environment| {
                (
                    environment.id.to_string(),
                    (
                        environment.name.clone(),
                        environment.facts.get("home").cloned(),
                    ),
                )
            })
            .collect();

    let filter = SessionFilter {
        limit: AGGREGATE_LIMIT,
        capability_subject: Some(subject.0.clone()),
        capability: Some("observe".to_string()),
        ..SessionFilter::default()
    };
    let sessions = state.store.list_sessions(&filter).await?;

    // Insertion-ordered accumulation, then sorted once. A BTreeMap would order
    // by path, but the sidebar wants recency, so the sort happens at the end.
    let mut index: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    let mut rows: Vec<Workspace> = Vec::new();

    for session in &sessions {
        let Some(workdir) = session.workdir.as_deref() else {
            continue;
        };
        let mut path = normalize(workdir);
        let environment = session.id.environment.to_string();
        // The envagent resolves Quick Start to `<environment home>/scratch`. Fold that
        // concrete path back into the explicit sentinel row so its counts remain
        // attached to the same environment-level workspace key.
        let is_quick_start = environment_meta
            .get(&environment)
            .and_then(|(_, home)| home.as_deref())
            .map(|home| normalize(&format!("{home}/scratch")) == path)
            .unwrap_or(false);
        if is_quick_start {
            path = QUICK_START_SENTINEL.to_string();
        }
        let at = *index
            .entry((environment.clone(), path.clone()))
            .or_insert_with(|| {
                rows.push(Workspace {
                    environment_name: environment_meta
                        .get(&environment)
                        .map(|meta| meta.0.clone()),
                    home: environment_meta
                        .get(&environment)
                        .and_then(|meta| meta.1.clone()),
                    environment,
                    name: if is_quick_start {
                        "Quick Start".to_string()
                    } else {
                        leaf(&path).to_string()
                    },
                    is_quick_start,
                    path,
                    session_count: 0,
                    running_count: 0,
                    last_used_ms: 0,
                });
                rows.len() - 1
            });
        let row = &mut rows[at];
        row.session_count += 1;
        if session.status == "running" {
            row.running_count += 1;
        }
        row.last_used_ms = row.last_used_ms.max(session.updated_at_ms);
    }

    // Quick Start is an environment-level action. Give every paired environment its
    // own explicit row, including disconnected ones, so selecting/pinning/history
    // switching always carries the environment id. The sentinel is intentionally
    // not an absolute server path: a remote envagent owns the directory it resolves.
    for environment in &environments {
        let id = environment.id.to_string();
        if rows
            .iter()
            .any(|row| row.environment == id && row.is_quick_start)
        {
            continue;
        }
        rows.push(Workspace {
            environment: id,
            environment_name: Some(environment.name.clone()),
            home: environment.facts.get("home").cloned(),
            path: QUICK_START_SENTINEL.to_string(),
            name: "Quick Start".to_string(),
            session_count: 0,
            running_count: 0,
            last_used_ms: 0,
            is_quick_start: true,
        });
    }

    rows.sort_by(|a, b| {
        b.last_used_ms
            .cmp(&a.last_used_ms)
            .then(a.path.cmp(&b.path))
            .then(a.environment.cmp(&b.environment))
    });

    Ok(Json(WorkspacesResponse {
        quick_start_dir,
        data_dir: state.config.data_dir.display().to_string(),
        // There is no single home once multiple environments are paired. Clients
        // should read `Workspace.home` for the selected environment.
        home: String::new(),
        workspaces: rows,
    }))
}

/// Drop trailing separators so `/tmp` and `/tmp/` are one workspace.
///
/// Grouping is by string, not by inode: two spellings of the same directory would
/// otherwise appear as two rows in the sidebar. Root is preserved.
///
/// [`sylph_protocol::normalize_workdir`] is the rule, and it is shared with the adapters
/// for a reason this function used to demonstrate: the adapters compare a session's
/// recorded workdir against a requested one when enumerating a native store, so the two
/// sides must normalize identically or a trailing slash silently hides a directory's
/// history. There were four copies and the adapters' three disagreed with this one about
/// the filesystem root.
pub fn normalize(workdir: &str) -> String {
    sylph_protocol::normalize_workdir(workdir).to_string()
}

/// Last path component, for display.
fn leaf(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_slashes_group_as_one_workspace() {
        assert_eq!(normalize("/tmp/"), "/tmp");
        assert_eq!(normalize("/tmp"), "/tmp");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize(""), "/");
    }

    #[test]
    fn leaf_names_the_directory() {
        assert_eq!(leaf("/Users/x/Workspace/sylph-web"), "sylph-web");
        assert_eq!(leaf("/Users/x/.sylph/scratch"), "scratch");
        // Root has no leaf; falling back to the path keeps the label non-empty.
        assert_eq!(leaf("/"), "/");
    }
}
