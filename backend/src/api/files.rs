//! Read-only file browsing for the Artifacts pane.
//!
//! # Threat model
//!
//! This endpoint turns a client-supplied string into a filesystem read, so it is
//! the one place in the API where path handling is a security boundary rather
//! than a convenience. Two rules make it safe, and both are enforced on the
//! *canonical* path, never on the string:
//!
//! 1. **The root is the session's `workdir`, taken from the journal.** The
//!    client names a session, not a directory, so no request can pick its own
//!    root. `..` in the relative path is not rejected by pattern-matching —
//!    which is defeatable by encoding — but by canonicalizing the joined path
//!    and confirming it is still inside the canonical root.
//! 2. **Symlinks are resolved before the containment check.** `std::fs::canonicalize`
//!    resolves every component, so a symlink inside the workdir pointing at
//!    `/etc/passwd` canonicalizes outside the root and is refused. This is why
//!    the check cannot use `Path::components` filtering alone: that inspects the
//!    request, and the escape lives on disk.
//!
//! Both operations are read-only. There is deliberately no write, rename, or
//! delete endpoint: the agent mutates the workspace, the browser observes it.

use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as AxumPath, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    AppState,
};

/// Largest file served inline.
///
/// The Artifacts pane previews source files and images, and a browser cannot
/// usefully render megabytes of text in a side panel anyway. Beyond this the
/// response reports the size and sets `truncated`, so the UI can say why it is
/// showing a prefix instead of failing.
const MAX_PREVIEW_BYTES: u64 = 1024 * 1024;

/// How much of an oversized text file to return.
const TRUNCATE_TO_BYTES: usize = 256 * 1024;

/// Entries per directory listing.
///
/// A `node_modules` directory can hold tens of thousands of entries; sending
/// them all would stall the pane for a list nobody reads to the end.
const MAX_ENTRIES: usize = 2000;

/// Directory names never listed.
///
/// These are large, machine-generated, and never the artifact a user is looking
/// for. `.git` is excluded for a second reason: it holds object data and config
/// that has no business being browsable from a web pane.
const SKIPPED_DIRS: &[&str] = &[
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Path relative to the session's workdir, `/`-separated. The root itself
    /// is the empty string.
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    /// Byte size for files; omitted for directories, where it means nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    pub modified_at_ms: i64,
    /// True when the entry is a symlink. Followed links that leave the root are
    /// omitted from the listing entirely, so a listed symlink is always safe.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_symlink: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeResponse {
    /// Absolute path of the directory listed, for display and context menus.
    pub root: String,
    /// The requested directory, relative to the workdir.
    pub path: String,
    pub entries: Vec<FileEntry>,
    /// True when the directory holds more than [`MAX_ENTRIES`] entries.
    pub truncated: bool,
}

#[derive(Deserialize)]
pub struct TreeQuery {
    /// Directory to list, relative to the session workdir. Defaults to the root.
    #[serde(default)]
    path: String,
}

/// GET /api/v1/sessions/{id}/files?path=...
///
/// One directory level per request. Lazy expansion is what keeps a repository
/// with 100k files usable: the alternative, walking the tree server-side, spends
/// seconds building a payload the pane renders only the top of.
pub async fn tree(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<TreeQuery>,
) -> Result<Json<TreeResponse>> {
    let (root, target) = resolve(&state, &id, &query.path).await?;

    let metadata = std::fs::metadata(&target)
        .map_err(|e| describe_io_error(&e, "read that directory"))?;
    if !metadata.is_dir() {
        return Err(Error::InvalidRequest(
            "That path is a file, not a directory.".to_string(),
        ));
    }

    let mut entries = Vec::new();
    let mut truncated = false;
    let reader =
        std::fs::read_dir(&target).map_err(|e| describe_io_error(&e, "read that directory"))?;

    for entry in reader {
        let entry = match entry {
            Ok(entry) => entry,
            // A single unreadable entry must not fail the whole listing: a
            // directory with one permission-denied child is still browsable.
            Err(_) => continue,
        };
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }

        let name = entry.file_name().to_string_lossy().to_string();

        // Two different questions about one entry, and they need two different
        // calls. Conflating them is what made a symlinked directory unbrowsable:
        //
        //   *Is this entry a link?*      must NOT follow it.
        //   *What is on the other side?* must follow it.
        //
        // `DirEntry::file_type` answers the first from the directory entry, with
        // no extra syscall on platforms that fill in `d_type`. The second needs
        // `fs::metadata`, which traverses; `DirEntry::metadata` does not —
        // std documents it as "will not traverse symlinks if this entry points
        // at a symlink". Using it for the second question reported a symlinked
        // directory as `isDir: false` carrying the link's own byte length, so
        // the pane drew a tiny file with no expand arrow and no way to open it.
        let is_symlink = match entry.file_type() {
            Ok(file_type) => file_type.is_symlink(),
            Err(_) => continue,
        };

        // Resolve a link before trusting anything it says about itself. This is
        // both the containment check and the way to reach the target's real
        // kind, so it has to happen before the `is_dir` decision below — a link
        // that escapes the root must not even be classified, let alone listed.
        //
        // Only links pay for this. A regular entry sits under an already-canonical
        // directory, so its path is canonical too, and spending a `canonicalize`
        // on each of 2000 entries would buy nothing.
        let resolved = if is_symlink {
            match std::fs::canonicalize(entry.path()) {
                // A link out of the root would otherwise be an expandable node
                // offering the whole filesystem through this endpoint.
                Ok(target) if target.starts_with(&root) => target,
                // Dangling or escaping. Both are skipped, for the same reason:
                // there is nothing inside the workspace to show, and a listed
                // entry the client cannot open is worse than an absent one.
                _ => continue,
            }
        } else {
            entry.path()
        };

        // Following metadata: the kind and size the user is actually browsing.
        // For a link this describes the target, which is what makes a symlinked
        // directory expand like the directory it is.
        let metadata = match std::fs::metadata(&resolved) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        let is_dir = metadata.is_dir();
        // Checked against the entry's own name, not the target's: a `node_modules`
        // symlink is noise under the name it is browsed by, however it is spelled
        // on the other side.
        if is_dir && SKIPPED_DIRS.contains(&name.as_str()) {
            continue;
        }

        let relative = join_relative(&query.path, &name);
        entries.push(FileEntry {
            path: relative,
            name,
            is_dir,
            size: (!is_dir).then_some(metadata.len()),
            modified_at_ms: modified_ms(&metadata),
            is_symlink,
        });
    }

    // Directories first, then case-insensitive by name: the ordering every file
    // browser uses, and the one that makes a tree scannable.
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(Json(TreeResponse {
        root: root.display().to_string(),
        path: normalize_relative(&query.path),
        entries,
        truncated,
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentResponse {
    pub path: String,
    /// Absolute path, for "Copy path" and "Reveal in Finder".
    pub absolute_path: String,
    pub size: u64,
    /// `text`, `image`, or `binary`. The client picks a viewer from this rather
    /// than guessing from the extension, because the server has already read the
    /// bytes and knows whether they decode as UTF-8.
    pub encoding: String,
    /// MIME type, guessed from the extension. Advisory: used to choose an
    /// image viewer and a highlighter, never to decide access.
    pub mime_type: String,
    /// Text content, or base64 for images. Absent for binaries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// True when only a prefix is returned.
    pub truncated: bool,
    pub modified_at_ms: i64,
}

#[derive(Deserialize)]
pub struct ContentQuery {
    path: String,
}

/// GET /api/v1/sessions/{id}/files/content?path=...
pub async fn content(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ContentQuery>,
) -> Result<Json<ContentResponse>> {
    let (_root, target) = resolve(&state, &id, &query.path).await?;

    let metadata =
        std::fs::metadata(&target).map_err(|e| describe_io_error(&e, "read that file"))?;
    if metadata.is_dir() {
        return Err(Error::InvalidRequest(
            "That path is a directory, not a file.".to_string(),
        ));
    }

    let size = metadata.len();
    let mime_type = guess_mime(&target);
    let is_image = mime_type.starts_with("image/");

    // Images are all-or-nothing: a truncated image decodes to nothing useful,
    // so an oversized one reports its size and offers no preview.
    if is_image {
        if size > MAX_PREVIEW_BYTES {
            return Ok(Json(ContentResponse {
                path: normalize_relative(&query.path),
                absolute_path: target.display().to_string(),
                size,
                encoding: "binary".to_string(),
                mime_type,
                content: None,
                truncated: true,
                modified_at_ms: modified_ms(&metadata),
            }));
        }
        let bytes = std::fs::read(&target).map_err(|e| describe_io_error(&e, "read that file"))?;
        return Ok(Json(ContentResponse {
            path: normalize_relative(&query.path),
            absolute_path: target.display().to_string(),
            size,
            encoding: "image".to_string(),
            mime_type,
            content: Some(base64_encode(&bytes)),
            truncated: false,
            modified_at_ms: modified_ms(&metadata),
        }));
    }

    let read_limit = size.min(MAX_PREVIEW_BYTES) as usize;
    let bytes = read_prefix(&target, read_limit)?;

    // UTF-8 validity, not the extension, decides whether this is text. An
    // extension-based guess mislabels a `.log` holding binary output, and
    // refuses a perfectly readable file with no extension at all.
    match String::from_utf8(bytes) {
        Ok(text) => {
            let truncated = size > read_limit as u64;
            let content = if truncated && text.len() > TRUNCATE_TO_BYTES {
                // Cut on a character boundary; slicing by byte index would panic
                // in the middle of a multi-byte codepoint.
                let end = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|i| *i <= TRUNCATE_TO_BYTES)
                    .last()
                    .unwrap_or(0);
                text[..end].to_string()
            } else {
                text
            };
            Ok(Json(ContentResponse {
                path: normalize_relative(&query.path),
                absolute_path: target.display().to_string(),
                size,
                encoding: "text".to_string(),
                mime_type,
                content: Some(content),
                truncated,
                modified_at_ms: modified_ms(&metadata),
            }))
        }
        Err(_) => Ok(Json(ContentResponse {
            path: normalize_relative(&query.path),
            absolute_path: target.display().to_string(),
            size,
            encoding: "binary".to_string(),
            mime_type,
            content: None,
            truncated: false,
            modified_at_ms: modified_ms(&metadata),
        })),
    }
}

/// Read at most `limit` bytes, so an enormous file cannot be pulled into memory
/// just to be discarded by the size check afterwards.
fn read_prefix(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| describe_io_error(&e, "read that file"))?;
    let mut buffer = Vec::with_capacity(limit.min(64 * 1024));
    file.take(limit as u64)
        .read_to_end(&mut buffer)
        .map_err(|e| describe_io_error(&e, "read that file"))?;
    Ok(buffer)
}

/// Resolve a session-relative path to an absolute one inside the workdir.
///
/// Returns `(canonical_root, canonical_target)`. Every containment decision in
/// this module goes through here; no handler joins paths itself.
async fn resolve(state: &AppState, session_id: &str, relative: &str) -> Result<(PathBuf, PathBuf)> {
    let session = state
        .store
        .get_session(session_id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    // Canonicalizing the root too matters on macOS, where `/tmp` is a symlink to
    // `/private/tmp`: comparing a canonical target against a non-canonical root
    // would reject every path under a session created in `/tmp`.
    let root = std::fs::canonicalize(&session.workdir).map_err(|_| {
        Error::InvalidRequest(format!(
            "This session's working directory no longer exists: {}",
            session.workdir
        ))
    })?;

    let cleaned = normalize_relative(relative);
    if cleaned.is_empty() {
        return Ok((root.clone(), root));
    }

    let joined = root.join(&cleaned);
    let target = std::fs::canonicalize(&joined)
        .map_err(|e| describe_io_error(&e, "find that path"))?;

    // The containment check. `starts_with` on `Path` compares whole components,
    // so `/workdir-evil` does not match a root of `/workdir` the way a string
    // prefix test would.
    if !target.starts_with(&root) {
        return Err(Error::Forbidden(
            "That path is outside the session's working directory.".to_string(),
        ));
    }

    Ok((root, target))
}

/// Normalize a client-supplied relative path to `a/b/c` form.
///
/// This is tidying, not security: it strips leading slashes so an absolute-looking
/// request is treated as relative, drops `.` segments, and collapses empty ones.
/// `..` is deliberately left in place for [`resolve`] to catch by canonicalization,
/// because filtering it here would create the illusion that this function is the
/// defense. It is not.
fn normalize_relative(path: &str) -> String {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Append a child name to a relative directory path.
fn join_relative(parent: &str, name: &str) -> String {
    let parent = normalize_relative(parent);
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn modified_ms(metadata: &std::fs::Metadata) -> i64 {
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
fn describe_io_error(error: &std::io::Error, action: &str) -> Error {
    match error.kind() {
        std::io::ErrorKind::NotFound => Error::FileNotFound,
        std::io::ErrorKind::PermissionDenied => Error::Forbidden(format!(
            "Permission denied trying to {action}."
        )),
        _ => Error::Internal(format!("Failed to {action}: {error}")),
    }
}

/// Extension-based MIME guess, for choosing a viewer and a highlighter.
///
/// Deliberately a small table rather than a dependency: the Artifacts pane needs
/// to tell images from text and pick a syntax mode, and a full MIME database
/// would add a dependency for entries no coding workspace encounters.
fn guess_mime(path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mime = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/vnd.microsoft.icon",
        "avif" => "image/avif",
        "md" | "markdown" => "text/markdown",
        "json" => "application/json",
        "jsonl" | "ndjson" => "application/x-ndjson",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "csv" => "text/csv",
        "rs" => "text/rust",
        "ts" | "tsx" => "text/typescript",
        "js" | "jsx" | "mjs" | "cjs" => "text/javascript",
        "py" => "text/x-python",
        "sh" | "bash" | "zsh" => "text/x-shellscript",
        "go" => "text/x-go",
        "sql" => "application/sql",
        "pdf" => "application/pdf",
        "" => "text/plain",
        _ => "text/plain",
    };
    mime.to_string()
}

/// Minimal base64 encoder for image previews.
///
/// Written out rather than pulled in: `base64` is not currently a dependency,
/// and this is the only encode site in the backend. Decoding attachments uses
/// its own path in `attachments.rs`.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
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
        // `..` survives on purpose: canonicalization in `resolve` is the check,
        // and stripping it here would hide traversal attempts from that check.
        assert_eq!(normalize_relative("../etc/passwd"), "../etc/passwd");
    }

    #[test]
    fn join_relative_builds_child_paths() {
        assert_eq!(join_relative("", "src"), "src");
        assert_eq!(join_relative("src", "main.rs"), "src/main.rs");
        assert_eq!(join_relative("/src/", "main.rs"), "src/main.rs");
    }

    /// A sibling directory sharing a prefix must not be reachable. This is the
    /// bug a string `starts_with` would introduce.
    #[test]
    fn component_wise_containment_rejects_prefix_siblings() {
        let root = Path::new("/tmp/work");
        assert!(Path::new("/tmp/work/src/main.rs").starts_with(root));
        assert!(!Path::new("/tmp/work-evil/secret").starts_with(root));
    }

    #[test]
    fn images_are_detected_by_extension() {
        assert_eq!(guess_mime(Path::new("a/b.png")), "image/png");
        assert_eq!(guess_mime(Path::new("a/B.JPEG")), "image/jpeg");
        assert_eq!(guess_mime(Path::new("a/readme.md")), "text/markdown");
        assert_eq!(guess_mime(Path::new("Makefile")), "text/plain");
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
        // Bytes above 0x7f exercise the shift arithmetic, where a sign-extension
        // bug would show up.
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
    }
}
