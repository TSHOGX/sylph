//! Path containment: the security boundary for every filesystem endpoint.
//!
//! Two endpoints turn client-supplied strings into filesystem reads, and they
//! have different roots — [`crate::api::files`] is rooted at one session's
//! `workdir`, [`crate::api::fs`] at the configured browse roots. The *root*
//! differs; the *check* must not. This module holds that check once, because a
//! containment rule implemented twice is a containment rule that will diverge.
//!
//! The rule: canonicalize, then compare component-wise.
//!
//! Both halves matter. Canonicalizing resolves `..` and every symlink component,
//! so traversal is defeated by resolution rather than by pattern-matching the
//! request — pattern-matching is defeatable by encoding, and cannot see an
//! escape that lives on disk as a symlink. Comparing with [`Path::starts_with`]
//! rather than string prefixes compares whole components, so `/workdir-evil`
//! does not pass a root of `/workdir`.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Directory names never listed or descended.
///
/// These are large, machine-generated, and never the artifact a user is looking
/// for. `.git` is excluded for a second reason: it holds object data and config
/// that has no business being browsable over HTTP.
pub const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".next",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    "dist",
    "build",
];

/// Normalize a client-supplied relative path to `a/b/c` form.
///
/// This is tidying, not security: it strips leading slashes so an absolute-looking
/// request is treated as relative, drops `.` segments, and collapses empty ones.
/// `..` is deliberately left in place for [`contain`] to catch by canonicalization,
/// because filtering it here would create the illusion that this function is the
/// defense. It is not.
pub fn normalize_relative(path: &str) -> String {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Append a child name to a relative directory path.
pub fn join_relative(parent: &str, name: &str) -> String {
    let parent = normalize_relative(parent);
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// Canonicalize `candidate` and require the result to sit inside `root`.
///
/// `root` must already be canonical — see [`canonical_root`]. Returns the
/// canonical target so callers operate on the resolved path rather than the
/// string they were handed.
pub fn contain(root: &Path, candidate: &Path) -> Result<PathBuf> {
    let target = std::fs::canonicalize(candidate)
        .map_err(|e| describe_io_error(&e, "find that path"))?;
    if !target.starts_with(root) {
        return Err(Error::Forbidden(
            "That path is outside the permitted directory.".to_string(),
        ));
    }
    Ok(target)
}

/// Canonicalize a configured or journalled root.
///
/// Canonicalizing the root matters on macOS, where `/tmp` is a symlink to
/// `/private/tmp`: comparing a canonical target against a non-canonical root
/// would reject every path beneath it.
pub fn canonical_root(path: &Path, describe: &str) -> Result<PathBuf> {
    std::fs::canonicalize(path).map_err(|_| {
        Error::InvalidRequest(format!("{describe} does not exist: {}", path.display()))
    })
}

/// Milliseconds since the epoch for a file's mtime, or 0 when unavailable.
pub fn modified_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Turn an IO error into an API error that names the cause.
///
/// `NotFound` and `PermissionDenied` are the two a user can act on, and they
/// need different statuses: a missing file is a 404, a refused one is not.
/// Everything else is internal, and its message is withheld because IO error
/// strings carry absolute paths.
pub fn describe_io_error(error: &std::io::Error, action: &str) -> Error {
    match error.kind() {
        std::io::ErrorKind::NotFound => Error::FileNotFound,
        std::io::ErrorKind::PermissionDenied => {
            Error::Forbidden(format!("Permission denied trying to {action}."))
        }
        _ => Error::Internal(format!("Failed to {action}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_normalized_without_pretending_to_be_safe() {
        assert_eq!(normalize_relative("/src/main.rs"), "src/main.rs");
        assert_eq!(normalize_relative("src//main.rs"), "src/main.rs");
        assert_eq!(normalize_relative("./src/./main.rs"), "src/main.rs");
        assert_eq!(normalize_relative(""), "");
        assert_eq!(normalize_relative("/"), "");
        // `..` survives on purpose: canonicalization in `contain` is the check,
        // and stripping it here would hide traversal attempts from that check.
        assert_eq!(normalize_relative("../etc/passwd"), "../etc/passwd");
    }

    #[test]
    fn join_relative_builds_child_paths() {
        assert_eq!(join_relative("", "src"), "src");
        assert_eq!(join_relative("src", "main.rs"), "src/main.rs");
        assert_eq!(join_relative("/src/", "main.rs"), "src/main.rs");
    }

    /// A sibling directory sharing a name prefix must not be reachable. This is
    /// the bug a string `starts_with` would introduce.
    #[test]
    fn containment_rejects_prefix_siblings() {
        let root = std::env::temp_dir().join("sylph-contain-root");
        let sibling = std::env::temp_dir().join("sylph-contain-root-evil");
        std::fs::create_dir_all(root.join("inside")).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let root = std::fs::canonicalize(&root).unwrap();
        assert!(contain(&root, &root.join("inside")).is_ok());
        assert!(contain(&root, &sibling).is_err());

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&sibling).ok();
    }

    /// Traversal is caught after resolution, not by inspecting the request.
    #[test]
    fn containment_rejects_traversal_after_canonicalization() {
        let root = std::env::temp_dir().join("sylph-contain-traverse");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();

        assert!(contain(&root, &root.join("sub/../..")).is_err());

        std::fs::remove_dir_all(&root).ok();
    }

    /// A symlink pointing out of the root canonicalizes outside it and is
    /// refused. This is the escape a components-only filter cannot see.
    #[cfg(unix)]
    #[test]
    fn containment_rejects_escaping_symlinks() {
        let root = std::env::temp_dir().join("sylph-contain-symlink");
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let link = root.join("escape");
        std::fs::remove_file(&link).ok();
        std::os::unix::fs::symlink("/etc", &link).unwrap();

        assert!(contain(&root, &link).is_err());

        std::fs::remove_dir_all(&root).ok();
    }
}
