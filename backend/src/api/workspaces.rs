//! Workspace roll-up for the left sidebar and the settings panel.
//!
//! The frontend groups sessions by working directory to form "spaces". It could
//! do that arithmetic itself from `GET /sessions`, but two things are only
//! knowable here: the absolute path of the Quick Start directory (`~/.agents`),
//! and counts that span more sessions than a `LIMIT`-capped listing returns.
//! Deriving the home directory in the browser is impossible, and hardcoding
//! `~/.agents` client-side would break the moment `HOME` differs from the
//! user Next.js runs as.

use axum::{extract::State, Json};
use serde::Serialize;

use crate::{db::store::SessionFilter, error::Result, AppState};

/// One working directory that has at least one session, or is the Quick Start
/// directory. `path` is absolute: the sidebar shortens it for display, but
/// every API call that follows needs the real path.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
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
    /// Absolute path of the Quick Start directory (`$HOME/.agents`).
    pub quick_start_dir: String,
    /// Where the journal and attachments live. Shown read-only in settings.
    pub data_dir: String,
    pub home: String,
    /// Newest first, Quick Start included so settings can show its counts.
    pub workspaces: Vec<Workspace>,
}

/// How many sessions to aggregate over.
///
/// Higher than the session-list default because this endpoint reports counts:
/// a truncated scan would understate a busy workspace, and an understated count
/// is worse than a slow one. The journal is local SQLite, so the scan is cheap.
const AGGREGATE_LIMIT: i64 = 2000;

/// GET /api/v1/workspaces
pub async fn list(State(state): State<AppState>) -> Result<Json<WorkspacesResponse>> {
    let home = std::env::var("HOME").unwrap_or_default();
    let quick_start_dir = format!("{home}/.agents");

    let filter = SessionFilter {
        limit: AGGREGATE_LIMIT,
        ..SessionFilter::default()
    };
    let sessions = state.store.list_sessions(&filter).await?;

    // Insertion-ordered accumulation, then sorted once. A BTreeMap would order
    // by path, but the sidebar wants recency, so the sort happens at the end.
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut rows: Vec<Workspace> = Vec::new();

    for session in &sessions {
        let path = normalize(&session.workdir);
        let at = *index.entry(path.clone()).or_insert_with(|| {
            rows.push(Workspace {
                name: leaf(&path).to_string(),
                is_quick_start: path == quick_start_dir,
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

    // Quick Start is always present, even with no sessions: the sidebar section
    // and the settings row both exist before the first quick session is made.
    if !index.contains_key(&quick_start_dir) {
        rows.push(Workspace {
            path: quick_start_dir.clone(),
            name: leaf(&quick_start_dir).to_string(),
            session_count: 0,
            running_count: 0,
            last_used_ms: 0,
            is_quick_start: true,
        });
    }

    rows.sort_by(|a, b| b.last_used_ms.cmp(&a.last_used_ms).then(a.path.cmp(&b.path)));

    Ok(Json(WorkspacesResponse {
        quick_start_dir,
        data_dir: state.config.data_dir.display().to_string(),
        home,
        workspaces: rows,
    }))
}

/// Drop trailing separators so `/tmp` and `/tmp/` are one workspace.
///
/// Grouping is by string, not by inode: two spellings of the same directory
/// would otherwise appear as two spaces in the sidebar. Root is preserved.
fn normalize(workdir: &str) -> String {
    let trimmed = workdir.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Last path component, for display.
fn leaf(path: &str) -> &str {
    path.rsplit('/').find(|part| !part.is_empty()).unwrap_or(path)
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
        assert_eq!(leaf("/Users/x/.agents"), ".agents");
        // Root has no leaf; falling back to the path keeps the label non-empty.
        assert_eq!(leaf("/"), "/");
    }
}
