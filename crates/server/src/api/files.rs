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
//! Reads are unrestricted within the root. The mutating operations — [`upload`],
//! [`mkdir`], [`remove`] — are each confined to the same subtree by the same
//! resolver, and each refuses the destructive default: an upload will not
//! overwrite unless asked, a mkdir will not replace, and a delete will not
//! recurse unless asked and will not touch the root at all. There is still no
//! rename or write-in-place: the agent mutates file *contents*, the browser
//! manages the files around it.

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

enum RemoteTarget {
    Draft {
        environment: sylph_protocol::EnvironmentId,
        draft_id: String,
        workdir: String,
    },
    Session {
        environment: sylph_protocol::EnvironmentId,
        session_id: sylph_protocol::SessionId,
        workdir: String,
    },
}

async fn remote_target(state: &AppState, id: &str) -> Result<Option<RemoteTarget>> {
    if id.starts_with("remote-draft-") {
        let Some(draft) = state.env_bridge.remote_draft(id).await else {
            return Err(Error::SessionNotFound);
        };
        return Ok(Some(RemoteTarget::Draft {
            environment: draft.environment,
            draft_id: id.to_string(),
            workdir: draft.workdir,
        }));
    }
    if id.starts_with("draft-") {
        return Err(Error::SessionNotFound);
    }
    // Every durable session is owned by an envagent, including one whose
    // environment happens to share the server host. The server never resolves
    // or reads a session workdir locally.
    let session_id = crate::api::sessions::decode_id(id)?;
    let Some(session) = state.store.get_session(&session_id).await? else {
        return Err(Error::SessionNotFound);
    };
    Ok(Some(RemoteTarget::Session {
        environment: session_id.environment.clone(),
        session_id,
        workdir: session.workdir,
    }))
}

fn remote_parts(
    target: &RemoteTarget,
) -> (
    &sylph_protocol::EnvironmentId,
    Option<String>,
    Option<sylph_protocol::SessionId>,
    &str,
) {
    match target {
        RemoteTarget::Draft {
            environment,
            draft_id,
            workdir,
        } => (environment, Some(draft_id.clone()), None, workdir),
        RemoteTarget::Session {
            environment,
            session_id,
            workdir,
        } => (environment, None, Some(session_id.clone()), workdir),
    }
}

fn map_remote_file_error(error: anyhow::Error) -> Error {
    let text = format!("{error:#}");
    if text.contains("DRAFT_NOT_FOUND") || text.contains("SESSION_NOT_HOSTED") {
        Error::LinkGone
    } else if text.contains("outside") || text.contains("escapes") {
        Error::Forbidden("That path is outside the permitted directory.".into())
    } else if text.contains("No such file") || text.contains("not found") {
        Error::FileNotFound
    } else {
        Error::LinkFailed(text)
    }
}

async fn remote_read_bytes(
    state: &AppState,
    target: &RemoteTarget,
    relative: String,
) -> Result<Vec<u8>> {
    let (environment, draft_id, session_id, _) = remote_parts(target);
    let value = state
        .env_bridge
        .remote_files(
            environment,
            draft_id,
            session_id,
            sylph_protocol::FileOperation::Read {
                relative,
                limit: None,
            },
        )
        .await
        .map_err(map_remote_file_error)?;
    serde_json::from_value(
        value
            .get("bytes")
            .cloned()
            .ok_or_else(|| Error::Internal("remote read returned no bytes".into()))?,
    )
    .map_err(|error| Error::Internal(format!("remote read returned malformed bytes: {error}")))
}

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
    /// True when the directory holds more than `MAX_ENTRIES` entries.
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
    if let Some(remote) = remote_target(&state, &id).await? {
        let (environment, draft_id, session_id, workdir) = remote_parts(&remote);
        let value = state
            .env_bridge
            .remote_files(
                environment,
                draft_id,
                session_id,
                sylph_protocol::FileOperation::List {
                    relative: query.path.clone(),
                },
            )
            .await
            .map_err(map_remote_file_error)?;
        let raw_entries = value.as_array().ok_or_else(|| {
            Error::Internal("remote file listing returned a malformed payload".into())
        })?;
        let mut entries = Vec::with_capacity(raw_entries.len());
        for entry in raw_entries {
            let object = entry.as_object().ok_or_else(|| {
                Error::Internal("remote file listing contained a malformed entry".into())
            })?;
            let name = object
                .get("name")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Error::Internal("remote file entry has no name".into()))?
                .to_string();
            let is_dir = object
                .get("directory")
                .and_then(|value| value.as_bool())
                .ok_or_else(|| Error::Internal("remote file entry has no directory flag".into()))?;
            let size = if is_dir {
                None
            } else {
                Some(
                    object
                        .get("size")
                        .and_then(|value| value.as_u64())
                        .ok_or_else(|| Error::Internal("remote file entry has no size".into()))?,
                )
            };
            entries.push(FileEntry {
                path: paths::join_relative(&query.path, &name),
                name: name.clone(),
                is_dir,
                size,
                modified_at_ms: object
                    .get("modifiedAtMs")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0),
                is_symlink: object
                    .get("symlink")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                preview_kind: (!is_dir)
                    .then(|| crate::api::files::preview_kind(Path::new(&name)))
                    .flatten(),
            });
        }
        return Ok(Json(TreeResponse {
            root: workdir.to_string(),
            path: paths::normalize_relative(&query.path),
            entries,
            truncated: false,
        }));
    }
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
    let reader = std::fs::read_dir(&target)
        .map_err(|e| paths::describe_io_error(&e, "read that directory"))?;

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
    if let Some(remote) = remote_target(&state, &id).await? {
        let path = paths::normalize_relative(&query.path);
        let bytes = remote_read_bytes(&state, &remote, path.clone()).await?;
        let size = bytes.len() as u64;
        let mime_type = guess_mime(Path::new(&path));
        let is_image = mime_type.starts_with("image/");
        if is_image {
            return Ok(Json(ContentResponse {
                path,
                absolute_path: format!("{}/{}", remote_parts(&remote).3, query.path),
                size,
                encoding: "image".into(),
                mime_type,
                content: Some(base64_encode(&bytes)),
                truncated: false,
                modified_at_ms: 0,
            }));
        }
        match String::from_utf8(bytes) {
            Ok(text) => Ok(Json(ContentResponse {
                path,
                absolute_path: format!("{}/{}", remote_parts(&remote).3, query.path),
                size,
                encoding: "text".into(),
                mime_type,
                content: Some(text),
                truncated: false,
                modified_at_ms: 0,
            })),
            Err(_) => Ok(Json(ContentResponse {
                path,
                absolute_path: format!("{}/{}", remote_parts(&remote).3, query.path),
                size,
                encoding: "binary".into(),
                mime_type,
                content: None,
                truncated: false,
                modified_at_ms: 0,
            })),
        }
    } else {
        return Err(Error::SessionNotFound);
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
    if let Some(remote) = remote_target(&state, &id).await? {
        let path = paths::normalize_relative(&query.path);
        let bytes = remote_read_bytes(&state, &remote, path.clone()).await?;
        if bytes.len() as u64 > documents::MAX_DOCUMENT_BYTES {
            return Err(Error::InvalidRequest(format!(
                "This file is over the {} MiB document preview limit.",
                documents::MAX_DOCUMENT_BYTES / 1024 / 1024
            )));
        }
        // The parser is intentionally shared with local files.  The temporary
        // copy is bounded by the envagent read limit and lives under the
        // server's own temp directory; no remote path is ever opened by the
        // server.
        let suffix = Path::new(&path)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{value}"))
            .unwrap_or_default();
        let temporary_path = state.config.data_dir.join(format!(
            "sylph-remote-document-{}{}",
            uuid::Uuid::new_v4().simple(),
            suffix
        ));
        std::fs::create_dir_all(&state.config.data_dir).map_err(|error| {
            Error::Internal(format!("Could not stage remote document: {error}"))
        })?;
        std::fs::write(&temporary_path, &bytes).map_err(|error| {
            Error::Internal(format!("Could not stage remote document: {error}"))
        })?;
        let window = Window::new(query.sheet, query.row, query.rows);
        let parse_path = temporary_path.clone();
        let parsed_result =
            tokio::task::spawn_blocking(move || documents::parse(&parse_path, window))
                .await
                .map_err(|error| {
                    tracing::error!("remote document parser panicked: {error}");
                    Error::Internal("Failed to parse that remote document.".into())
                })?;
        let _ = std::fs::remove_file(&temporary_path);
        let parsed = parsed_result?;
        let (_, _, _, workdir) = remote_parts(&remote);
        return Ok(Json(DocumentResponse {
            path,
            absolute_path: format!("{}/{}", workdir, query.path),
            size: bytes.len() as u64,
            modified_at_ms: 0,
            document: parsed,
        }));
    }
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
/// Bytes sniffed to decide whether an unrecognized file is text.
///
/// A prefix, not the whole file: the question is "would a browser tab show
/// something readable", and a file whose first 64 KB decode as UTF-8 answers it.
/// Reading the rest to be certain would make a 304 — which sends no body — pay
/// for a full read of a file it is not going to send.
const SNIFF_BYTES: usize = 64 * 1024;

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

#[derive(Deserialize)]
pub struct RawQuery {
    path: String,
    /// Force a download instead of letting the browser render the file.
    ///
    /// Separate from the type allowlist because the two answer different
    /// questions: the allowlist decides what is *safe* to render, this decides
    /// what the user *asked* for. Without it, "Download" on a PNG would open a
    /// tab showing the image, which is the one thing the user did not want.
    #[serde(default)]
    download: bool,
}

/// GET /api/v1/sessions/{id}/files/raw?path=&download=
///
/// The file's bytes, for content a browser renders natively: images, HTML, PDF,
/// and any file whose bytes turn out to be text.
///
/// # Why this is the "open in a new tab" route
///
/// The pane's viewer is bounded on purpose — `MAX_PREVIEW_BYTES` and
/// `TRUNCATE_TO_BYTES` keep a 40 MB log from being pushed through JSON into a
/// 320px panel. But "the preview is truncated" should not mean "you cannot see
/// this file", so the same file is reachable here in full, rendered by the
/// browser's own viewer, which streams, finds, and zooms better than anything
/// this pane would reimplement. Nothing is truncated on this route.
///
/// Whether that lands as a rendered tab or a download is decided by type, and
/// only ever downwards: an unrecognized type is never *promoted* into something
/// the browser will execute.
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
    Query(query): Query<RawQuery>,
    headers: HeaderMap,
) -> Result<axum::response::Response> {
    if let Some(remote) = remote_target(&state, &id).await? {
        let path = paths::normalize_relative(&query.path);
        let bytes = remote_read_bytes(&state, &remote, path.clone()).await?;
        let mime = guess_mime(Path::new(&path));
        let inline = !query.download
            && (INLINE_TYPES.contains(&mime.as_str())
                || (!bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok()));
        let content_type = if inline {
            if INLINE_TYPES.contains(&mime.as_str()) {
                mime
            } else {
                "text/plain; charset=utf-8".into()
            }
        } else {
            "application/octet-stream".into()
        };
        let filename = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");
        let etag = format!("\"remote-{}\"", blake3::hash(&bytes).to_hex());
        if headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
        {
            return Ok((
                StatusCode::NOT_MODIFIED,
                hardened_headers(&etag, &content_type, inline, Some(filename)),
            )
                .into_response());
        }
        return Ok((
            hardened_headers(&etag, &content_type, inline, Some(filename)),
            bytes,
        )
            .into_response());
    }
    let (_root, target) = resolve(&state, &id, &query.path).await?;

    let metadata =
        std::fs::metadata(&target).map_err(|e| paths::describe_io_error(&e, "read that file"))?;
    if metadata.is_dir() {
        return Err(Error::InvalidRequest(
            "That path is a directory, not a file.".to_string(),
        ));
    }

    let (content_type, mut inline) = serve_type(&target);
    // An explicit download overrides a renderable type, never the reverse.
    if query.download {
        inline = false;
    }
    // The name the browser saves under. Taken from the *canonical* path rather
    // than from the request, so nothing client-supplied is reflected into a
    // response header.
    let filename = target
        .file_name()
        .map(|name| name.to_string_lossy().to_string());

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
        return Ok((
            StatusCode::NOT_MODIFIED,
            hardened_headers(&etag, &content_type, inline, filename.as_deref()),
        )
            .into_response());
    }

    let bytes =
        std::fs::read(&target).map_err(|e| paths::describe_io_error(&e, "read that file"))?;

    Ok((
        hardened_headers(&etag, &content_type, inline, filename.as_deref()),
        bytes,
    )
        .into_response())
}

/// The `Content-Type` this file goes out as, and whether it renders inline.
///
/// Three tiers, narrowest first:
///
/// 1. An allowlisted type is served as itself — the browser has a real viewer
///    for it, and [`hardened_headers`] is what makes that safe.
/// 2. Anything else that *is text* is served as `text/plain`, never as its own
///    `text/…` label. A tab showing source is the whole point of this route, and
///    `text/plain` cannot execute whatever the extension claimed the file was.
/// 3. Everything else downloads.
///
/// Tier 2 asks the bytes, not the extension. An extension table would need an
/// entry per language to keep `.rb` or `.svelte` from downloading, and would
/// still be wrong about a `.log` full of binary — the same reason [`content`]
/// decides text-vs-binary by decoding rather than by name.
fn serve_type(path: &Path) -> (String, bool) {
    let mime = guess_mime(path);
    if INLINE_TYPES.contains(&mime.as_str()) {
        return (mime, true);
    }
    if looks_like_text(path) {
        return ("text/plain; charset=utf-8".to_string(), true);
    }
    // Not on the allowlist and not text: refuse to name a type the browser
    // might render.
    ("application/octet-stream".to_string(), false)
}

/// Whether a prefix of this file decodes as UTF-8.
///
/// A trailing `Utf8Error` with no `error_len` is an *incomplete* codepoint at the
/// cut, not invalid data, so it counts as text — otherwise a multi-byte character
/// straddling the 64 KB boundary would demote a perfectly readable file to a
/// download, with the outcome depending on where in the file it happened to sit.
fn looks_like_text(path: &Path) -> bool {
    let Ok(bytes) = read_prefix(path, SNIFF_BYTES) else {
        return false;
    };
    // A NUL is the one byte no text file has and every binary format does. Cheap
    // to check, and it rejects UTF-16 and most binaries before the decode.
    if bytes.contains(&0) {
        return false;
    }
    match std::str::from_utf8(&bytes) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none() && error.valid_up_to() > 0,
    }
}

/// The header set every [`raw`] response carries, including a 304.
///
/// Built in one place so a `Not Modified` cannot accidentally ship without the
/// sandbox policy that the 200 has.
fn hardened_headers(
    etag: &str,
    content_type: &str,
    inline: bool,
    filename: Option<&str>,
) -> HeaderMap {
    let mut headers = HeaderMap::new();

    // An attachment is never labelled with a renderable type: if the browser is
    // saving it, naming the type buys nothing and a sniffed rename costs.
    let content_type = if inline {
        content_type
    } else {
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
    // The filename is included now, unlike the first version of this route,
    // because a download without one saves as `raw` with no extension — the
    // browser has nothing else to name it from. What made that unsafe was
    // reflecting *client* text into a header; this name comes from the canonical
    // path and is then encoded rather than trusted.
    let disposition = match filename {
        Some(name) => content_disposition(inline, name),
        None => (if inline { "inline" } else { "attachment" }).to_string(),
    };
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, private"),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }

    headers
}

/// A `Content-Disposition` value carrying both spellings of the filename.
///
/// RFC 6266 wants the plain `filename=` for old clients and `filename*=` for
/// anything non-ASCII, and every current browser prefers the latter. Both are
/// sent because a CJK or emoji filename — ordinary in this project's own
/// workspaces — cannot be expressed in the plain parameter at all: a bare
/// `filename="报告.pdf"` is an invalid header value that `HeaderValue::from_str`
/// rejects, which would have silently dropped the disposition entirely.
fn content_disposition(inline: bool, filename: &str) -> String {
    let kind = if inline { "inline" } else { "attachment" };
    // The ASCII fallback: quotes and backslashes would end the parameter early,
    // and control characters cannot appear in a header value.
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{}",
        percent_encode(filename)
    )
}

/// Percent-encode for RFC 5987, keeping only the unreserved set.
///
/// Hand-rolled rather than adding `percent-encoding`: this is the only caller in
/// the codebase, and the conservative rule — encode everything that is not
/// `A-Za-z0-9-._~` — is shorter than the argument for a dependency.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
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
    let file =
        std::fs::File::open(path).map_err(|e| paths::describe_io_error(&e, "read that file"))?;
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
    // A prepared link has a working directory before it has a durable identity.  The
    // handle is deliberately accepted only when it has the server-generated `draft-`
    // shape; every other value still goes through the strict identity decoder below.
    // This keeps the file API useful while composing without pretending a draft handle
    // is a session id (the original bug sent `draft-...` through `SessionId::decode`).
    let workdir = if session_id.starts_with("draft-") {
        state
            .links
            .prepared_spec(session_id)
            .await
            .ok_or(Error::SessionNotFound)?
            .workdir
    } else {
        state
            .store
            .get_session(&crate::api::sessions::decode_id(session_id)?)
            .await?
            .ok_or(Error::SessionNotFound)?
            .workdir
    };

    // Canonicalizing the root too matters on macOS, where `/tmp` is a symlink to
    // `/private/tmp`: comparing a canonical target against a non-canonical root
    // would reject every path under a session created in `/tmp`.
    let root = std::fs::canonicalize(&workdir).map_err(|_| {
        Error::InvalidRequest(format!(
            "This session's working directory no longer exists: {}",
            workdir
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
    if let Some(remote) = remote_target(&state, &id).await? {
        let filename = crate::attachments::sanitize_filename(&req.name);
        let relative = paths::join_relative(&req.path, &filename);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(req.data.as_bytes())
            .map_err(|error| Error::AttachmentInvalid(format!("Invalid base64: {error}")))?;
        if bytes.len() > crate::attachments::MAX_ATTACHMENT_BYTES {
            return Err(Error::AttachmentInvalid(format!(
                "'{}' is {} bytes (max {})",
                req.name,
                bytes.len(),
                crate::attachments::MAX_ATTACHMENT_BYTES
            )));
        }
        let (environment, draft_id, session_id, workdir) = remote_parts(&remote);
        let value = state
            .env_bridge
            .remote_files(
                environment,
                draft_id,
                session_id,
                sylph_protocol::FileOperation::Write {
                    relative: relative.clone(),
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    overwrite: req.overwrite,
                },
            )
            .await
            .map_err(map_remote_file_error)?;
        let size = value
            .get("written")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        return Ok((
            StatusCode::CREATED,
            Json(UploadResponse {
                path: relative.clone(),
                absolute_path: format!("{workdir}/{relative}"),
                size,
            }),
        ));
    }
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

    // `symlink_metadata`, so an existing entry is inspected rather than followed.
    // This is where the containment established on the *parent* stops being
    // sufficient: the last component is deliberately left unresolved (the file
    // usually does not exist yet, and `canonicalize` needs an existing path), so a
    // pre-existing symlink there is inside the root by name and points wherever it
    // likes. `tokio::fs::write` follows it, which turns `overwrite` into a write
    // outside the workdir — the exact escape the check exists to prevent, arriving
    // through the one component the check could not see.
    //
    // `remove` is safe from this by construction, because unlinking a link never
    // touches its target, and its doc comment asserts that a final escaping
    // symlink is "only ever unlinked, never opened or written to". That invariant
    // was true of `remove` and false of this function; it is now true of both.
    //
    // Resolved and re-checked rather than refused outright: a symlink pointing
    // *inside* the workdir is ordinary (a linked fixture, a shared config), and
    // refusing every link would deny a write the agent driving this session can
    // plainly make itself.
    match std::fs::symlink_metadata(&target) {
        Ok(existing) => {
            if !req.overwrite {
                return Err(Error::InvalidRequest(format!(
                    "{filename} already exists. Send overwrite to replace it."
                )));
            }
            if existing.file_type().is_symlink() {
                // A broken link canonicalizes to nothing, so there is no target to
                // prove contained — refuse rather than create the file it points at.
                let resolved = paths::contain(&root, &target).map_err(|error| match error {
                    Error::FileNotFound => Error::Forbidden(format!(
                        "{filename} is a symbolic link that does not resolve, so where a \
                         write would land cannot be established."
                    )),
                    other => other,
                })?;
                debug_assert!(resolved.starts_with(&root));
            }
        }
        // Nothing there: the ordinary create path.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(paths::describe_io_error(&error, "read that path")),
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MkdirRequest {
    /// Parent directory, relative to the workdir. Empty means the root.
    #[serde(default)]
    pub path: String,
    /// New directory's name; reduced to one path component.
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MkdirResponse {
    pub path: String,
    pub absolute_path: String,
}

/// POST /api/v1/sessions/{id}/files/directory
///
/// Create one directory inside the session's working directory.
///
/// Non-recursive on purpose: `create_dir_all` would turn a name with a typo'd
/// separator into a silent two-level tree, and the sanitizer means a separator
/// cannot survive anyway — so the recursive variant could only ever hide a bug.
/// It refuses an existing path rather than succeeding quietly, so "New folder"
/// twice reports the collision instead of appearing to have done nothing.
pub async fn mkdir(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<MkdirRequest>,
) -> Result<(StatusCode, Json<MkdirResponse>)> {
    if let Some(remote) = remote_target(&state, &id).await? {
        let name = sanitize_component(&req.name)?;
        let relative = paths::join_relative(&req.path, &name);
        let (environment, draft_id, session_id, workdir) = remote_parts(&remote);
        state
            .env_bridge
            .remote_files(
                environment,
                draft_id,
                session_id,
                sylph_protocol::FileOperation::Mkdir {
                    relative: relative.clone(),
                },
            )
            .await
            .map_err(map_remote_file_error)?;
        return Ok((
            StatusCode::CREATED,
            Json(MkdirResponse {
                path: relative,
                absolute_path: format!("{workdir}/{}", paths::join_relative(&req.path, &name)),
            }),
        ));
    }
    let (root, parent) = resolve(&state, &id, &req.path).await?;

    let metadata = std::fs::metadata(&parent)
        .map_err(|e| paths::describe_io_error(&e, "read that directory"))?;
    if !metadata.is_dir() {
        return Err(Error::InvalidRequest(
            "The parent of a new folder must be a directory.".to_string(),
        ));
    }

    let name = sanitize_component(&req.name)?;
    let target = parent.join(&name);
    // Containment re-asserted on the joined path, for the same reason `upload`
    // does it: one comparison, versus trusting a sanitizer at a distance.
    if !target.starts_with(&root) {
        return Err(Error::Forbidden(
            "That path is outside the session's working directory.".to_string(),
        ));
    }
    if target.exists() {
        return Err(Error::InvalidRequest(format!("{name} already exists.")));
    }

    tokio::fs::create_dir(&target)
        .await
        .map_err(|e| paths::describe_io_error(&e, "create that folder"))?;

    Ok((
        StatusCode::CREATED,
        Json(MkdirResponse {
            path: paths::join_relative(&req.path, &name),
            absolute_path: target.display().to_string(),
        }),
    ))
}

#[derive(Deserialize)]
pub struct RemoveQuery {
    /// Path to delete, relative to the workdir.
    path: String,
    /// Required to delete a directory that is not empty.
    #[serde(default)]
    recursive: bool,
}

/// DELETE /api/v1/sessions/{id}/files?path=&recursive=
///
/// Delete one file, or one directory.
///
/// # Why this is permitted, and where it stops
///
/// This is the only irreversible operation in the API: there is no trash to
/// restore from, because reaching the platform's trash means either a new
/// dependency or spawning `osascript`, and a browser page that can invoke a host
/// process is a larger concession than the delete itself. So the guards are the
/// safety, and they are chosen against the ways this actually goes wrong:
///
/// - **The root is refused.** `path=""` normalizes to the workdir, and a client
///   bug that sent an empty path would otherwise erase the user's project. There
///   is no legitimate request that means "delete the working directory".
/// - **A non-empty directory needs `recursive`.** Deleting one stray file and
///   deleting a subtree are different intentions; the client asks the user which
///   one they meant, and the default answer here is the narrow one.
/// - **Confirmation is the client's job, and it does it** — but the guards above
///   hold whether or not the caller is this project's UI.
/// - **A symlink is unlinked, never followed.** See `resolve_parent` for why
///   that requires resolving the parent instead of the target.
pub async fn remove(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<RemoveQuery>,
) -> Result<StatusCode> {
    let cleaned = paths::normalize_relative(&query.path);
    if cleaned.is_empty() {
        return Err(Error::Forbidden(
            "The session's working directory cannot be deleted.".to_string(),
        ));
    }

    if let Some(remote) = remote_target(&state, &id).await? {
        let (environment, draft_id, session_id, _) = remote_parts(&remote);
        state
            .env_bridge
            .remote_files(
                environment,
                draft_id,
                session_id,
                sylph_protocol::FileOperation::Remove {
                    relative: cleaned,
                    recursive: query.recursive,
                },
            )
            .await
            .map_err(map_remote_file_error)?;
        return Ok(StatusCode::NO_CONTENT);
    }

    let (_root, target) = resolve_parent(&state, &id, &cleaned).await?;

    // `symlink_metadata`, not `metadata`: a symlink must be unlinked, not walked.
    // This is only *able* to see the link because `resolve_parent` left the last
    // component unresolved — through `resolve` the path would already have been
    // canonicalized to the target, and deleting a link to a populated directory
    // would have deleted the directory.
    let metadata = std::fs::symlink_metadata(&target)
        .map_err(|e| paths::describe_io_error(&e, "read that path"))?;

    if metadata.is_dir() {
        let empty = std::fs::read_dir(&target)
            .map_err(|e| paths::describe_io_error(&e, "read that directory"))?
            .next()
            .is_none();
        if !empty && !query.recursive {
            return Err(Error::InvalidRequest(
                "That folder is not empty. Send recursive to delete it and everything in it."
                    .to_string(),
            ));
        }
        tokio::fs::remove_dir_all(&target)
            .await
            .map_err(|e| paths::describe_io_error(&e, "delete that folder"))?;
    } else {
        tokio::fs::remove_file(&target)
            .await
            .map_err(|e| paths::describe_io_error(&e, "delete that file"))?;
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Resolve a path whose *last component must not be followed*.
///
/// [`resolve`] canonicalizes the whole path, which is right for reading — a
/// symlink is transparent, and resolving it is what the containment check needs
/// to see. It is wrong for deleting: canonicalized, `link` becomes the directory
/// it points at, and unlinking becomes recursive deletion of the target. That is
/// not a hypothetical; it is what the first version of [`remove`] did, and the
/// symlink test caught it.
///
/// So the parent is canonicalized and contained, and the final component is
/// appended to it verbatim. Containment still holds: every `..` and every
/// symlinked *directory* in the path is resolved and checked, and a final
/// component that is itself an escaping symlink is only ever unlinked, never
/// opened or walked.
async fn resolve_parent(
    state: &AppState,
    session_id: &str,
    cleaned: &str,
) -> Result<(PathBuf, PathBuf)> {
    let (parent, name) = match cleaned.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", cleaned),
    };
    // Rejects the names that would re-target the operation at a directory the
    // caller did not name — `..` most of all, which would climb.
    let name = sanitize_component(name)?;

    let (root, directory) = resolve(state, session_id, parent).await?;
    let target = directory.join(&name);
    if !target.starts_with(&root) {
        return Err(Error::Forbidden(
            "That path is outside the session's working directory.".to_string(),
        ));
    }
    Ok((root, target))
}

/// Reduce a user-supplied name to exactly one path component.
///
/// Deliberately *not* [`crate::attachments::sanitize_filename`], which strips
/// every non-ASCII character: that is right for a filename derived from an
/// upload, where the name is incidental, and wrong for a folder the user is
/// naming, where `文档` would silently become `attachment`. So this rejects
/// rather than rewrites — the user gets to fix their own name — and rejects only
/// what a path component cannot contain.
fn sanitize_component(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidRequest("A name is required.".to_string()));
    }
    if trimmed.len() > 255 {
        return Err(Error::InvalidRequest(
            "That name is too long for a directory entry.".to_string(),
        ));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(Error::InvalidRequest(
            "That name refers to a directory that already exists.".to_string(),
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(Error::InvalidRequest(
            "A name cannot contain a path separator.".to_string(),
        ));
    }
    // A NUL terminates the string the OS actually receives, so a name containing
    // one creates a different path than the one shown in the request.
    if trimmed.chars().any(|c| c == '\0' || c.is_control()) {
        return Err(Error::InvalidRequest(
            "A name cannot contain control characters.".to_string(),
        ));
    }
    Ok(trimmed.to_string())
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

    /// The three tiers of [`serve_type`], each pinned by the outcome that would
    /// be a defect: a PDF that downloads, source that downloads, a binary that
    /// renders.
    #[test]
    fn serve_type_renders_allowlisted_and_textual_files_and_downloads_the_rest() {
        let dir = std::env::temp_dir().join(format!("sylph-serve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let pdf = dir.join("a.pdf");
        std::fs::write(&pdf, b"%PDF-1.4 not really").unwrap();
        assert_eq!(serve_type(&pdf), ("application/pdf".to_string(), true));

        // An extension with no MIME entry at all: the bytes decide, and they are
        // text. This is the case an extension table gets wrong.
        let source = dir.join("main.svelte");
        std::fs::write(&source, "<script>let a = 1;</script>").unwrap();
        assert_eq!(
            serve_type(&source),
            ("text/plain; charset=utf-8".to_string(), true)
        );

        // A `.log` holding binary output — the inverse mistake.
        let binary = dir.join("core.log");
        std::fs::write(&binary, [0x00, 0x01, 0xff, 0xfe]).unwrap();
        assert_eq!(
            serve_type(&binary),
            ("application/octet-stream".to_string(), false)
        );

        // HTML keeps its own type, because a rendered page is the point. It is
        // safe only because of the sandbox header; see `raw`.
        let page = dir.join("report.html");
        std::fs::write(&page, "<p>hi</p>").unwrap();
        assert_eq!(serve_type(&page), ("text/html".to_string(), true));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A codepoint split by the sniff boundary is incomplete data, not invalid
    /// data. Getting this wrong makes a file's renderability depend on where a
    /// multi-byte character happens to fall.
    #[test]
    fn a_codepoint_straddling_the_sniff_boundary_still_counts_as_text() {
        let dir = std::env::temp_dir().join(format!("sylph-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.txt");

        let mut content = "a".repeat(SNIFF_BYTES - 1);
        content.push('中'); // 3 bytes, so it is cut after the first
        content.push_str(&"b".repeat(100));
        std::fs::write(&path, &content).unwrap();

        assert!(looks_like_text(&path));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn content_disposition_carries_both_filename_spellings() {
        assert_eq!(
            content_disposition(false, "report.pdf"),
            "attachment; filename=\"report.pdf\"; filename*=UTF-8''report.pdf"
        );
        // The case the plain parameter cannot express, and the reason both are
        // sent: the ASCII fallback degrades, the encoded one is exact.
        assert_eq!(
            content_disposition(true, "报告.pdf"),
            "inline; filename=\"__.pdf\"; filename*=UTF-8''%E6%8A%A5%E5%91%8A.pdf"
        );
        // A quote would otherwise end the parameter and let the rest of the name
        // be read as header syntax.
        assert!(!content_disposition(false, "a\"b.txt").contains("a\"b"));
    }

    /// Rejecting beats rewriting for a name the user typed: `文档` must not
    /// become `attachment`, and `../etc` must not become `etc`.
    #[test]
    fn component_names_are_rejected_rather_than_rewritten() {
        assert_eq!(sanitize_component(" src ").unwrap(), "src");
        assert_eq!(sanitize_component("文档").unwrap(), "文档");
        assert_eq!(sanitize_component("a.b-c_d").unwrap(), "a.b-c_d");

        for bad in ["", "   ", ".", "..", "a/b", "a\\b", "a\0b", "a\nb"] {
            assert!(
                sanitize_component(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(sanitize_component(&"x".repeat(300)).is_err());
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
