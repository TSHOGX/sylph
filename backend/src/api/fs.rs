//! Machine-level directory browsing, for choosing a workspace to bind.
//!
//! # Why this exists
//!
//! A workspace *is* a directory, and a session cannot be created without one.
//! But nothing in a browser can name a directory on the machine the backend runs
//! on: `~` cannot be expanded client-side, and [`crate::api::files`] — the only
//! other listing endpoint — is rooted at an existing session's `workdir`, so it
//! cannot help name the first one. Without this module the loop is closed:
//! binding a new project directory is impossible from the UI.
//!
//! # Threat model
//!
//! This is the only endpoint that lists directories the *client* names rather
//! than ones derived from a session, so the bound cannot come from the request.
//! It comes from configuration: [`crate::config::Config::browse_roots`], which
//! is `$HOME` plus anything in `SYLPH_WEB_BROWSE_ROOTS`. Every request must
//! resolve inside one of those roots, checked on the canonical path by
//! [`crate::paths::contain`] — the same primitive the session-scoped file API
//! uses, deliberately shared rather than reimplemented.
//!
//! Three further restrictions keep this narrower than the session file API:
//!
//! 1. **Directories only.** Files are not listed. The purpose is picking a
//!    directory, and omitting files means this endpoint cannot be used to
//!    discover, say, which private keys exist by name.
//! 2. **No content endpoint.** There is deliberately no `/fs/content`. Reading
//!    bytes stays session-scoped, where the root is journalled rather than
//!    chosen. Enumerating directory names is a strictly smaller disclosure than
//!    serving file contents, and this module stays on that side of the line.
//! 3. **Escaping symlinks are omitted,** not merely refused on access, so a link
//!    out of the root is not even advertised as a node to try.
//!
//! Dotfiles *are* listed, unlike a conventional file browser. `~/.agents` is the
//! Quick Start directory and a legitimate workspace, so hiding dot-directories
//! would hide a directory the product itself points at.

use std::path::PathBuf;

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    paths::{self, SKIPPED_DIRS},
    AppState,
};

/// Entries per directory listing, mirroring the session file API's cap.
const MAX_ENTRIES: usize = 2000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    /// Display label: the directory leaf, or `Home` for the home directory.
    pub name: String,
    pub path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootsResponse {
    pub roots: Vec<Root>,
    /// Absolute home directory, so the client can abbreviate paths as `~/…`
    /// without guessing.
    pub home: String,
}

/// GET /api/v1/fs/roots
///
/// The set of directories browsing may start from. A root that no longer exists
/// is filtered out rather than reported: a stale `SYLPH_WEB_BROWSE_ROOTS` entry
/// should not present the user an unopenable node.
pub async fn roots(State(state): State<AppState>) -> Result<Json<RootsResponse>> {
    let home = std::env::var("HOME").unwrap_or_default();

    let roots = state
        .config
        .browse_roots
        .iter()
        .filter_map(|path| {
            let canonical = std::fs::canonicalize(path).ok()?;
            if !canonical.is_dir() {
                return None;
            }
            let display = canonical.display().to_string();
            let name = if display == home {
                "Home".to_string()
            } else {
                leaf(&display)
            };
            Some(Root {
                name,
                path: display,
            })
        })
        .collect();

    Ok(Json(RootsResponse { roots, home }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntryOut {
    /// Absolute path, because the client hands this straight back as a session
    /// `workdir` or as the next directory to list.
    pub path: String,
    pub name: String,
    pub modified_at_ms: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_symlink: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseResponse {
    /// The directory listed, canonicalized.
    pub path: String,
    /// Parent directory, absent when `path` is itself a configured root — which
    /// is what stops the client offering a "go up" affordance that would leave
    /// the permitted set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Subdirectories only, directories-first ordering being moot here.
    pub entries: Vec<DirEntryOut>,
    pub truncated: bool,
}

#[derive(Deserialize)]
pub struct BrowseQuery {
    /// Absolute path to list. Defaults to the first configured root.
    #[serde(default)]
    path: String,
}

/// GET /api/v1/fs/tree?path=...
///
/// One directory level per request, subdirectories only.
pub async fn tree(
    State(state): State<AppState>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<BrowseResponse>> {
    let allowed = canonical_roots(&state);
    if allowed.is_empty() {
        return Err(Error::Internal(
            "No browsable directories are configured.".to_string(),
        ));
    }

    // An empty path means "start somewhere sensible" rather than being an error,
    // so the picker's first request needs no prior knowledge of the roots.
    let requested = if query.path.trim().is_empty() {
        allowed[0].clone()
    } else {
        PathBuf::from(query.path.trim())
    };

    let target = contain_in_any(&allowed, &requested)?;

    let metadata = std::fs::metadata(&target)
        .map_err(|e| paths::describe_io_error(&e, "read that directory"))?;
    if !metadata.is_dir() {
        return Err(Error::InvalidRequest(
            "That path is a file, not a directory.".to_string(),
        ));
    }

    let mut entries = Vec::new();
    let mut truncated = false;
    let reader = std::fs::read_dir(&target)
        .map_err(|e| paths::describe_io_error(&e, "read that directory"))?;

    for entry in reader {
        // A single unreadable entry must not fail the whole listing: a directory
        // with one permission-denied child is still browsable.
        let Ok(entry) = entry else { continue };
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }

        let name = entry.file_name().to_string_lossy().to_string();

        // `file_type` answers "is this entry a link?" without traversing;
        // `fs::metadata` below answers "what is on the other side?" by
        // traversing. Conflating the two misreports a symlinked directory.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_symlink = file_type.is_symlink();

        // Resolve a link before trusting anything it says, and drop it if it
        // leaves the permitted set — an escaping link must not even be offered
        // as an expandable node.
        let resolved = if is_symlink {
            match contain_in_any(&allowed, &entry.path()) {
                Ok(resolved) => resolved,
                Err(_) => continue,
            }
        } else {
            entry.path()
        };

        let Ok(metadata) = std::fs::metadata(&resolved) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        // Checked against the entry's own name: `node_modules` is noise under the
        // name it is browsed by, however it is spelled on the other side.
        if SKIPPED_DIRS.contains(&name.as_str()) {
            continue;
        }

        entries.push(DirEntryOut {
            path: entry.path().display().to_string(),
            name,
            modified_at_ms: paths::modified_ms(&metadata),
            is_symlink,
        });
    }

    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    // Offered only when it is still inside a root, so the picker cannot walk out
    // of the permitted set one level at a time.
    let parent = target
        .parent()
        .filter(|parent| allowed.iter().any(|root| parent.starts_with(root)))
        .map(|parent| parent.display().to_string());

    Ok(Json(BrowseResponse {
        path: target.display().to_string(),
        parent,
        entries,
        truncated,
    }))
}

/// Canonicalized, still-existing browse roots.
fn canonical_roots(state: &AppState) -> Vec<PathBuf> {
    state
        .config
        .browse_roots
        .iter()
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .filter(|path| path.is_dir())
        .collect()
}

/// Require `candidate` to resolve inside at least one root.
///
/// Multiple roots mean containment is a disjunction, but each disjunct is the
/// same shared check — no root gets a weaker rule than another.
fn contain_in_any(roots: &[PathBuf], candidate: &std::path::Path) -> Result<PathBuf> {
    let mut last = None;
    for root in roots {
        match paths::contain(root, candidate) {
            Ok(target) => return Ok(target),
            Err(error) => last = Some(error),
        }
    }
    // A path that exists but sits outside every root reports Forbidden; one that
    // does not exist reports the IO error, so a typo is distinguishable from a
    // refusal.
    Err(match last {
        Some(Error::FileNotFound) => Error::FileNotFound,
        Some(Error::Forbidden(_)) | Some(_) | None => Error::Forbidden(
            "That directory is outside the browsable roots.".to_string(),
        ),
    })
}

/// Last path component, for display.
fn leaf(path: &str) -> String {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_names_the_directory() {
        assert_eq!(leaf("/Users/x/Workspace"), "Workspace");
        assert_eq!(leaf("/Users/x/.agents"), ".agents");
        assert_eq!(leaf("/"), "/");
    }

    #[test]
    fn containment_across_roots_accepts_any_and_refuses_outsiders() {
        let base = std::env::temp_dir().join("sylph-fs-roots");
        let a = base.join("a");
        let b = base.join("b");
        let outside = std::env::temp_dir().join("sylph-fs-outside");
        std::fs::create_dir_all(a.join("child")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let roots = vec![
            std::fs::canonicalize(&a).unwrap(),
            std::fs::canonicalize(&b).unwrap(),
        ];

        assert!(contain_in_any(&roots, &a.join("child")).is_ok());
        assert!(contain_in_any(&roots, &b).is_ok());
        assert!(contain_in_any(&roots, &outside).is_err());
        // Traversal out of one root is not rescued by the presence of another.
        assert!(contain_in_any(&roots, &a.join("../../etc")).is_err());

        std::fs::remove_dir_all(&base).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
