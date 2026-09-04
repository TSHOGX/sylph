//! Filesystem containment evaluated on the machine that owns the path.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContainmentError {
    #[error("path does not exist: {0}")]
    Missing(String),
    #[error("path is not a directory: {0}")]
    NotDirectory(String),
    #[error("path escapes every configured browse root: {0}")]
    Outside(String),
    #[error("cannot inspect path {path}: {reason}")]
    Unreadable { path: String, reason: String },
    #[error("refusing to follow a symlinked write target outside its parent root: {0}")]
    SymlinkEscape(String),
}

/// The envagent's local path policy.
///
/// The home directory is always included. Additional roots are explicit and
/// canonicalized once at construction, so every later check uses the same
/// component-wise comparison.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    roots: Vec<PathBuf>,
}

impl PathPolicy {
    pub fn new(
        home: impl AsRef<Path>,
        extra_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, ContainmentError> {
        let mut roots = Vec::new();
        let home = canonical_dir(home.as_ref())?;
        roots.push(home);
        for root in extra_roots {
            let canonical = canonical_dir(&root)?;
            if !roots.iter().any(|existing| existing == &canonical) {
                roots.push(canonical);
            }
        }
        Ok(Self { roots })
    }

    pub fn from_roots(roots: impl IntoIterator<Item = PathBuf>) -> Result<Self, ContainmentError> {
        let mut canonical = Vec::new();
        for root in roots {
            let root = canonical_dir(&root)?;
            if !canonical.iter().any(|existing| existing == &root) {
                canonical.push(root);
            }
        }
        if canonical.is_empty() {
            return Err(ContainmentError::Outside(
                "no browse roots configured".into(),
            ));
        }
        Ok(Self { roots: canonical })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Canonicalize and authorize an existing path.
    pub fn contain(&self, requested: impl AsRef<Path>) -> Result<PathBuf, ContainmentError> {
        let requested = requested.as_ref();
        let canonical = requested.canonicalize().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ContainmentError::Missing(requested.display().to_string())
            } else {
                ContainmentError::Unreadable {
                    path: requested.display().to_string(),
                    reason: error.to_string(),
                }
            }
        })?;
        if !self.is_within(&canonical) {
            return Err(ContainmentError::Outside(canonical.display().to_string()));
        }
        Ok(canonical)
    }

    /// Authorize a path that may not exist yet.
    ///
    /// The parent is canonicalized and the final component is appended
    /// verbatim. A pre-existing symlink at the final component is inspected
    /// before use and must still resolve inside the same root.
    pub fn contain_write_target(
        &self,
        requested: impl AsRef<Path>,
    ) -> Result<PathBuf, ContainmentError> {
        let requested = requested.as_ref();
        let parent = requested.parent().unwrap_or_else(|| Path::new("."));
        let leaf = requested
            .file_name()
            .ok_or_else(|| ContainmentError::Outside(requested.display().to_string()))?;
        let canonical_parent = self.contain(parent)?;
        let target = canonical_parent.join(leaf);
        if let Ok(metadata) = std::fs::symlink_metadata(&target) {
            if metadata.file_type().is_symlink() {
                let link =
                    std::fs::read_link(&target).map_err(|error| ContainmentError::Unreadable {
                        path: target.display().to_string(),
                        reason: error.to_string(),
                    })?;
                let resolved_path = if link.is_absolute() {
                    link
                } else {
                    target.parent().unwrap_or_else(|| Path::new(".")).join(link)
                };
                let resolved = resolved_path
                    .canonicalize()
                    .map_err(|_| ContainmentError::SymlinkEscape(target.display().to_string()))?;
                if !self.is_within(&resolved) {
                    return Err(ContainmentError::SymlinkEscape(
                        target.display().to_string(),
                    ));
                }
            }
        }
        Ok(target)
    }

    pub fn contains(&self, path: impl AsRef<Path>) -> bool {
        self.contain(path).is_ok()
    }

    fn is_within(&self, path: &Path) -> bool {
        self.roots
            .iter()
            .any(|root| path == root || path.starts_with(root))
    }
}

fn canonical_dir(path: &Path) -> Result<PathBuf, ContainmentError> {
    let canonical = path.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ContainmentError::Missing(path.display().to_string())
        } else {
            ContainmentError::Unreadable {
                path: path.display().to_string(),
                reason: error.to_string(),
            }
        }
    })?;
    if !canonical.is_dir() {
        return Err(ContainmentError::NotDirectory(
            canonical.display().to_string(),
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_containment_rejects_dotdot_and_prefix_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let sibling = dir.path().join("root-old");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(root.join("ok.txt"), "ok").unwrap();
        let policy = PathPolicy::from_roots([root.clone()]).unwrap();
        assert!(policy.contain(root.join("ok.txt")).is_ok());
        assert!(policy.contain(root.join("..").join("root-old")).is_err());
        assert!(policy.contain(sibling.join("x")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn write_target_checks_a_preexisting_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(outside.join("file"), root.join("link")).unwrap();
        let policy = PathPolicy::new(root.parent().unwrap(), [root.clone()]).unwrap();
        assert!(matches!(
            policy.contain_write_target(root.join("link")),
            Err(ContainmentError::SymlinkEscape(_))
        ));
    }
}
