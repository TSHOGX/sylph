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
    let target =
        std::fs::canonicalize(candidate).map_err(|e| describe_io_error(&e, "find that path"))?;
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

/// Require `candidate` to resolve inside at least one of `roots`.
///
/// Several roots make containment a disjunction, but each disjunct is the same
/// [`contain`] call — no root gets a weaker rule than another, and adding a root
/// cannot change how any existing one is checked.
///
/// The two failure modes are kept apart on purpose, because they send an
/// investigation to different places: a path that does not resolve at all is the
/// client's typo and reports as such, while one that resolves outside every root
/// is a *policy* refusal and says which roots exist. An error naming the wrong
/// layer is the expensive kind — "that directory does not exist" for a directory
/// the user is looking at sends them to the filesystem instead of to
/// `config.toml`.
pub fn contain_in_any(roots: &[PathBuf], candidate: &Path, describe: &str) -> Result<PathBuf> {
    let mut saw_missing = false;
    for root in roots {
        match contain(root, candidate) {
            Ok(target) => return Ok(target),
            Err(Error::FileNotFound) => saw_missing = true,
            Err(_) => {}
        }
    }

    if saw_missing || std::fs::canonicalize(candidate).is_err() {
        return Err(Error::FileNotFound);
    }

    Err(Error::Forbidden(format!(
        "{} is outside {describe}: {}",
        candidate.display(),
        describe_roots(roots)
    )))
}

/// Render a root set for an error message, bounded so it stays readable.
fn describe_roots(roots: &[PathBuf]) -> String {
    const MAX: usize = 4;
    let shown: Vec<String> = roots
        .iter()
        .take(MAX)
        .map(|root| root.display().to_string())
        .collect();
    if roots.len() > MAX {
        format!("{}, and {} more", shown.join(", "), roots.len() - MAX)
    } else {
        shown.join(", ")
    }
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

    /// Several roots accept any of them, and a traversal out of one is not
    /// rescued by the presence of another.
    #[test]
    fn multi_root_containment_accepts_any_and_refuses_outsiders() {
        let base = std::env::temp_dir().join("sylph-paths-multi");
        let a = base.join("a");
        let b = base.join("b");
        let outside = std::env::temp_dir().join("sylph-paths-multi-outside");
        std::fs::create_dir_all(a.join("child")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let roots = vec![
            std::fs::canonicalize(&a).unwrap(),
            std::fs::canonicalize(&b).unwrap(),
        ];

        assert!(contain_in_any(&roots, &a.join("child"), "the roots").is_ok());
        assert!(contain_in_any(&roots, &b, "the roots").is_ok());
        assert!(contain_in_any(&roots, &outside, "the roots").is_err());
        assert!(contain_in_any(&roots, &a.join("../../etc"), "the roots").is_err());

        std::fs::remove_dir_all(&base).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// "Does not exist" and "exists but is out of bounds" need different
    /// answers, because they send the reader to different places: one to the
    /// filesystem, the other to `config.toml`.
    #[test]
    fn a_missing_path_and_a_refused_path_report_differently() {
        let root = std::env::temp_dir().join("sylph-paths-report");
        let outside = std::env::temp_dir().join("sylph-paths-report-outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let roots = vec![std::fs::canonicalize(&root).unwrap()];

        assert!(matches!(
            contain_in_any(&roots, Path::new("/definitely/not/here"), "the roots"),
            Err(Error::FileNotFound)
        ));

        let refused = contain_in_any(&roots, &outside, "the permitted roots")
            .expect_err("an existing path outside every root must be refused");
        let message = format!("{refused}");
        assert!(matches!(refused, Error::Forbidden(_)), "{message}");
        // Names the bound that refused it, so the fix is discoverable.
        assert!(message.contains("the permitted roots"), "{message}");
        assert!(
            message.contains(&root.display().to_string())
                || message.contains(&std::fs::canonicalize(&root).unwrap().display().to_string()),
            "the message lists what is permitted: {message}"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
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
