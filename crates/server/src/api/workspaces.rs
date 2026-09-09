//! Workspace roll-up for the left sidebar and the settings panel.
//!
//! The frontend groups sessions by working directory to form "spaces". It could
//! do that arithmetic itself from `GET /sessions`, but two things are only
//! knowable here: the absolute path of the Quick Start directory, and counts that
//! span more sessions than a `LIMIT`-capped listing returns. The browser cannot
//! expand `~`, and the directory is configurable — so a client that derived it
//! would be wrong for anyone who set `paths.quick_start_dir`, and for every
//! deployment where `HOME` differs from the user Next.js runs as.

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
    /// Absolute path of the Quick Start directory, resolved server-side.
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
pub async fn list(State(state): State<AppState>) -> Result<Json<WorkspacesResponse>> {
    // Both from configuration rather than the environment: `quick_start_dir` is a
    // setting, and `home` is the base the picker abbreviates paths against, which
    // must be the same one `browse_roots` was resolved with.
    //
    // Canonicalized, because a session's journalled `workdir` is: creation resolves
    // the client's path before recording it, so it can never contain a symlinked
    // component. Comparing a canonical journal value against a configured spelling
    // would report `isQuickStart: false` for every Quick Start session on macOS,
    // where `<home>` under `/var/folders` resolves to `/private/var/folders` —
    // splitting the Quick Start row from the very sessions it counts. Reported in
    // the same spelling for the same reason: the client hands this back as a
    // `workdir`, and a second spelling would be a second sidebar group.
    let quick_start_dir = normalize(
        &std::fs::canonicalize(&state.config.quick_start_dir)
            .unwrap_or_else(|_| state.config.quick_start_dir.clone())
            .display()
            .to_string(),
    );
    let home = state
        .config
        .browse_roots
        .first()
        .map(|path| path.display().to_string())
        .unwrap_or_default();

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

    rows.sort_by(|a, b| {
        b.last_used_ms
            .cmp(&a.last_used_ms)
            .then(a.path.cmp(&b.path))
    });

    Ok(Json(WorkspacesResponse {
        quick_start_dir,
        data_dir: state.config.data_dir.display().to_string(),
        home,
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
