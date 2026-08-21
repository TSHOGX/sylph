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
//! The containment check itself lives in [`crate::paths`], shared with the
//! machine-level browser in [`crate::api::fs`]: a containment rule implemented
//! twice is one that will diverge.
//!
//! Reads are unrestricted within the root. Writing is deliberately narrower —
//! [`upload`] is the only mutating operation, it creates files and refuses to
//! overwrite by default, and there is still no rename or delete endpoint. The
//! agent mutates the workspace; the browser adds inputs for it to read.

use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    Json,
};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::{
    documents::{self, DocumentKind, Window},
    error::{Error, Result},
    paths::{self, SKIPPED_DIRS},
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
    /// Which viewer this file wants: `grid`, `blocks`, `html`, `pdf`, `image`.
    ///
    /// Present so a click knows which endpoint to call without first fetching the
    /// file to find out. Advisory and extension-based — the document endpoint
    /// still validates the bytes, and a mislabelled file falls back to
    /// `/files/content`. Absent means "read it as text".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_kind: Option<&'static str>,
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
        .map_err(|e| paths::describe_io_error(&e, "read that directory"))?;
    if !metadata.is_dir() {
        return Err(Error::InvalidRequest(
            "That path is a file, not a directory.".to_string(),
        ));
    }

    let mut entries = Vec::new();
    let mut truncated = false;
    let reader =
        std::fs::read_dir(&target).map_err(|e| paths::describe_io_error(&e, "read that directory"))?;

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

        let relative = paths::join_relative(&query.path, &name);
        entries.push(FileEntry {
            path: relative,
            name,
            is_dir,
            size: (!is_dir).then_some(metadata.len()),
            modified_at_ms: paths::modified_ms(&metadata),
            is_symlink,
            preview_kind: (!is_dir).then(|| preview_kind(&resolved)).flatten(),
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
        path: paths::normalize_relative(&query.path),
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
        std::fs::metadata(&target).map_err(|e| paths::describe_io_error(&e, "read that file"))?;
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
                path: paths::normalize_relative(&query.path),
                absolute_path: target.display().to_string(),
                size,
                encoding: "binary".to_string(),
                mime_type,
                content: None,
                truncated: true,
                modified_at_ms: paths::modified_ms(&metadata),
            }));
        }
        let bytes = std::fs::read(&target).map_err(|e| paths::describe_io_error(&e, "read that file"))?;
        return Ok(Json(ContentResponse {
            path: paths::normalize_relative(&query.path),
            absolute_path: target.display().to_string(),
            size,
            encoding: "image".to_string(),
            mime_type,
            content: Some(base64_encode(&bytes)),
            truncated: false,
            modified_at_ms: paths::modified_ms(&metadata),
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
                path: paths::normalize_relative(&query.path),
                absolute_path: target.display().to_string(),
                size,
                encoding: "text".to_string(),
                mime_type,
                content: Some(content),
                truncated,
                modified_at_ms: paths::modified_ms(&metadata),
            }))
        }
        Err(_) => Ok(Json(ContentResponse {
            path: paths::normalize_relative(&query.path),
            absolute_path: target.display().to_string(),
            size,
            encoding: "binary".to_string(),
            mime_type,
            content: None,
            truncated: false,
            modified_at_ms: paths::modified_ms(&metadata),
        })),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentResponse {
    pub path: String,
    pub absolute_path: String,
    pub size: u64,
    pub modified_at_ms: i64,
    /// The parsed payload, flattened so `kind`, `grid`, and `blocks` sit at the
    /// top level rather than under a wrapper the client would have to unwrap.
    #[serde(flatten)]
    pub document: documents::Document,
}

#[derive(Deserialize)]
pub struct DocumentQuery {
    path: String,
    /// Worksheet index. Meaningless for documents and presentations.
    sheet: Option<usize>,
    /// First row of the window, absolute within the sheet.
    row: Option<usize>,
    /// Rows to return. Clamped rather than validated.
    rows: Option<usize>,
}

/// GET /api/v1/sessions/{id}/files/document?path=&sheet=&row=&rows=
///
/// A spreadsheet, CSV, Word document, or PowerPoint deck as structure rather than
/// bytes. Sibling to [`content`] rather than an extension of it: a spreadsheet
/// needs windowing and sheet-selection parameters that mean nothing for any other
/// file, and paging must not re-send the file's metadata each time.
pub async fn document(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DocumentQuery>,
) -> Result<Json<DocumentResponse>> {
    let (_root, target) = resolve(&state, &id, &query.path).await?;

    let metadata =
        std::fs::metadata(&target).map_err(|e| paths::describe_io_error(&e, "read that file"))?;
    if metadata.is_dir() {
        return Err(Error::InvalidRequest(
            "That path is a directory, not a document.".to_string(),
        ));
    }

    let window = Window::new(query.sheet, query.row, query.rows);

    // Parsing is CPU-bound and takes ~600 ms on a large sheet. Run in an async
    // handler it would occupy a Tokio worker for that whole time, stalling every
    // SSE stream this process is serving. The other handlers in this module do
    // synchronous `std::fs` calls, which is tolerable for a bounded small read;
    // a document parse is not the same thing.
    let parsed = {
        let target = target.clone();
        tokio::task::spawn_blocking(move || documents::parse(&target, window))
            .await
            // A panic in a parser must not take down the request handler. It is
            // still a bug, so it is logged rather than quietly mapped.
            .map_err(|error| {
                tracing::error!("document parser panicked: {error}");
                Error::Internal("Failed to parse that document.".to_string())
            })??
    };

    Ok(Json(DocumentResponse {
        path: paths::normalize_relative(&query.path),
        absolute_path: target.display().to_string(),
        size: metadata.len(),
        modified_at_ms: paths::modified_ms(&metadata),
        document: parsed,
    }))
}

/// Types served inline by [`raw`].
///
/// An allowlist, not a blocklist, and deliberately short. Everything absent from
/// it is served as `application/octet-stream` with `Content-Disposition:
/// attachment`, so an unrecognized file downloads rather than rendering.
///
/// `image/svg+xml` is **excluded on purpose**. An SVG served from this origin can
/// execute script; the same file loaded through an `<img>` element cannot. SVG
/// therefore stays on the [`content`] base64 path, and adding it here would
/// silently convert an inert preview into a script-execution surface.
const INLINE_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/vnd.microsoft.icon",
    "image/avif",
    "application/pdf",
    "text/html",
];

/// GET /api/v1/sessions/{id}/files/raw?path=
///
/// The file's bytes, for content a browser renders natively: images, HTML, PDF.
///
/// # Why a byte route is acceptable here
///
/// The image preview previously went out as base64 inside JSON, justified by not
/// wanting "a second unauthenticated path to the filesystem". This route is not
/// unauthenticated: it sits behind the same auth middleware and the same
/// canonical-path containment check as every other handler in this module. What
/// base64 actually cost was 33% inflation, a JSON parse before first paint, no
/// progressive decode, and no HTTP caching — and it cannot carry a PDF at all.
///
/// # Why the headers are load-bearing
///
/// The real risk is the browser treating a workspace file as privileged content
/// in this origin. That is not hypothetical: serving an agent-generated HTML file
/// from a same-origin route with no hardening, a script inside it reads
/// `document.cookie` and gets the `sylph_session` token, which authorizes reads
/// across the whole workdir. Measured in a headless browser before this was
/// written.
///
/// `Content-Security-Policy: sandbox` is what closes it — the response lands in
/// an opaque origin however it is loaded, including direct navigation, so the
/// script cannot reach this origin's DOM or cookies. `nosniff` closes the sibling
/// path where an `octet-stream` payload gets sniffed into `text/html`.
///
/// Removing either header silently reintroduces cookie disclosure, which is why
/// an integration test asserts both.
pub async fn raw(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ContentQuery>,
    headers: HeaderMap,
) -> Result<axum::response::Response> {
    let (_root, target) = resolve(&state, &id, &query.path).await?;

    let metadata =
        std::fs::metadata(&target).map_err(|e| paths::describe_io_error(&e, "read that file"))?;
    if metadata.is_dir() {
        return Err(Error::InvalidRequest(
            "That path is a directory, not a file.".to_string(),
        ));
    }

    let mime_type = guess_mime(&target);
    let inline = INLINE_TYPES.contains(&mime_type.as_str());

    // mtime and size, which is enough to notice an agent's edit. A content hash
    // would be stronger but would mean reading the file to answer a request that
    // may not need its body.
    let etag = format!(
        "\"{:x}-{:x}\"",
        paths::modified_ms(&metadata).max(0) as u64,
        metadata.len()
    );

    // `Cache-Control: no-cache` means "revalidate", not "do not store", so a
    // repeat view is a 304 rather than a re-download while still never showing a
    // stale file.
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
    {
        return Ok((StatusCode::NOT_MODIFIED, hardened_headers(&etag, &mime_type, inline)).into_response());
    }

    let bytes = std::fs::read(&target).map_err(|e| paths::describe_io_error(&e, "read that file"))?;

    Ok((hardened_headers(&etag, &mime_type, inline), bytes).into_response())
}

/// The header set every [`raw`] response carries, including a 304.
///
/// Built in one place so a `Not Modified` cannot accidentally ship without the
/// sandbox policy that the 200 has.
fn hardened_headers(etag: &str, mime_type: &str, inline: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();

    let content_type = if inline {
        mime_type
    } else {
        // Not on the allowlist: refuse to name a type the browser might render.
        "application/octet-stream"
    };
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }

    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // The header that removes the cookie-disclosure path. See [`raw`].
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static(if inline { "inline" } else { "attachment" }),
    );
    // A filename is deliberately not included: it is client-supplied text
    // reflected into a header, and the client already knows what it asked for.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, private"),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }

    headers
}

/// Whether a file is worth offering to the document endpoint.
///
/// Used by [`tree`] to hint the viewer family, so a click knows which endpoint to
/// call without a probe request. Advisory: extension-based, and the document
/// endpoint still validates.
fn preview_kind(path: &Path) -> Option<&'static str> {
    let extension = path.extension().and_then(|e| e.to_str())?;
    if let Some(format) = documents::DocumentFormat::from_extension(extension) {
        return Some(match format.kind() {
            DocumentKind::Grid => "grid",
            DocumentKind::Blocks => "blocks",
        });
    }
    let mime = guess_mime(path);
    if mime == "text/html" {
        return Some("html");
    }
    if mime == "application/pdf" {
        return Some("pdf");
    }
    // SVG is an image to the viewer but must not reach the raw route, so it is
    // reported as an image and served through the base64 path like the others.
    if mime.starts_with("image/") {
        return Some("image");
    }
    None
}

/// Read at most `limit` bytes, so an enormous file cannot be pulled into memory
/// just to be discarded by the size check afterwards.
fn read_prefix(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| paths::describe_io_error(&e, "read that file"))?;
    let mut buffer = Vec::with_capacity(limit.min(64 * 1024));
    file.take(limit as u64)
        .read_to_end(&mut buffer)
        .map_err(|e| paths::describe_io_error(&e, "read that file"))?;
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

    let cleaned = paths::normalize_relative(relative);
    if cleaned.is_empty() {
        return Ok((root.clone(), root));
    }

    // The containment check, shared with the machine-level browser so both
    // endpoints resolve and compare identically.
    let target = paths::contain(&root, &root.join(&cleaned))?;

    Ok((root, target))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRequest {
    /// Destination directory, relative to the workdir. Empty means the root.
    #[serde(default)]
    pub path: String,
    /// Filename as the user knows it; sanitized to one path component.
    pub name: String,
    /// File bytes, base64, no `data:` prefix — the same encoding turn
    /// attachments already use, so the client has one encoder.
    pub data: String,
    /// Replace an existing file. Absent or false refuses instead.
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResponse {
    /// Path relative to the workdir, as the Artifacts pane addresses files.
    pub path: String,
    pub absolute_path: String,
    pub size: u64,
}

/// POST /api/v1/sessions/{id}/files
///
/// Write one file into the session's working directory.
///
/// # Why writing is permitted here
///
/// Reading and writing are not symmetric risks, so this needs its own argument
/// rather than inheriting the read endpoint's. The agent driving this session
/// already writes anywhere under `workdir` — that is what a coding agent does —
/// so a browser upload confined to the same subtree grants no capability the
/// session did not already have. What it must not become is a general
/// file-writing service: hence the root is the journalled `workdir` and never a
/// client-supplied directory, the destination is resolved through the same
/// containment check as reads, and an existing file is refused rather than
/// silently replaced, so an upload cannot quietly destroy source the user did
/// not mean to touch.
pub async fn upload(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<UploadRequest>,
) -> Result<(StatusCode, Json<UploadResponse>)> {
    // Resolves the *parent directory*, not the target: the file may not exist
    // yet, and `canonicalize` requires an existing path. Containment is therefore
    // established on the directory, and the filename is reduced to a single
    // component that cannot re-introduce a separator.
    let (root, directory) = resolve(&state, &id, &req.path).await?;

    let metadata = std::fs::metadata(&directory)
        .map_err(|e| paths::describe_io_error(&e, "read that directory"))?;
    if !metadata.is_dir() {
        return Err(Error::InvalidRequest(
            "The upload destination is a file, not a directory.".to_string(),
        ));
    }

    let filename = crate::attachments::sanitize_filename(&req.name);
    let target = directory.join(&filename);

    // Re-assert containment on the joined path. `sanitize_filename` already
    // strips separators, so this cannot currently fail — it is kept because the
    // cost is one comparison and the alternative is trusting a sanitizer at a
    // distance from the boundary it protects.
    if !target.starts_with(&root) {
        return Err(Error::Forbidden(
            "That path is outside the session's working directory.".to_string(),
        ));
    }

    if target.exists() && !req.overwrite {
        return Err(Error::InvalidRequest(format!(
            "{filename} already exists. Send overwrite to replace it."
        )));
    }

    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        req.data.as_bytes(),
    )
    .map_err(|e| Error::AttachmentInvalid(format!("Invalid base64 in '{}': {e}", req.name)))?;

    // Same ceiling as a turn attachment: both are browser uploads over the same
    // transport, and two different limits would be arbitrary.
    if bytes.len() > crate::attachments::MAX_ATTACHMENT_BYTES {
        return Err(Error::AttachmentInvalid(format!(
            "'{}' is {} bytes (max {})",
            req.name,
            bytes.len(),
            crate::attachments::MAX_ATTACHMENT_BYTES
        )));
    }

    tokio::fs::write(&target, &bytes)
        .await
        .map_err(|e| paths::describe_io_error(&e, "write that file"))?;

    Ok((
        StatusCode::CREATED,
        Json(UploadResponse {
            path: paths::join_relative(&req.path, &filename),
            absolute_path: target.display().to_string(),
            size: bytes.len() as u64,
        }),
    ))
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

/// Base64 for an inline image preview.
///
/// Delegates to the `base64` crate, which is already a dependency and is what
/// `attachments.rs` decodes with. An earlier hand-rolled encoder lived here
/// justified by a comment claiming base64 was not a dependency; it was, so the
/// justification was false and the code was an unnecessary second implementation
/// of a standard transform.
fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_normalized_without_pretending_to_be_safe() {
        assert_eq!(paths::normalize_relative("/src/main.rs"), "src/main.rs");
        assert_eq!(paths::normalize_relative("src//main.rs"), "src/main.rs");
        assert_eq!(paths::normalize_relative("./src/./main.rs"), "src/main.rs");
        assert_eq!(paths::normalize_relative(""), "");
        assert_eq!(paths::normalize_relative("/"), "");
        // `..` survives on purpose: canonicalization in `resolve` is the check,
        // and stripping it here would hide traversal attempts from that check.
        assert_eq!(paths::normalize_relative("../etc/passwd"), "../etc/passwd");
    }

    #[test]
    fn join_relative_builds_child_paths() {
        assert_eq!(paths::join_relative("", "src"), "src");
        assert_eq!(paths::join_relative("src", "main.rs"), "src/main.rs");
        assert_eq!(paths::join_relative("/src/", "main.rs"), "src/main.rs");
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
