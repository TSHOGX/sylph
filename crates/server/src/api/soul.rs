//! `GET /api/v1/soul` — the identity text Sylph appends to an agent's prompt.
//!
//! This exists because the toggle that governs injection is meaningless without
//! it. Asking a user to decide whether to append `SOUL.md` to every session,
//! while giving them no way to read what it says, is asking them to consent to
//! something invisible — and the file lives in the Sylph home directory, so there
//! is nowhere else in the UI they could look.
//!
//! Read on request rather than cached at startup. `SOUL.md` is hand-edited
//! between sessions, and a cached copy would show a stale identity next to a
//! toggle claiming to control the live one. It is a few kilobytes read from the
//! page cache, so the cost of being correct here is not measurable.

use axum::{extract::State, Json};
use serde::Serialize;

use crate::{error::Result, paths, AppState};

/// Largest `SOUL.md` returned in full.
///
/// A prompt preamble is prose, and the real one is ~3 KiB. This bound is not
/// about the file being dangerous, it is about the response staying a preview:
/// something that grew to megabytes is a mistake worth seeing the head of, not
/// worth shipping to a browser in one string.
const MAX_PREVIEW_BYTES: usize = 128 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoulResponse {
    /// Whether a readable `SOUL.md` was resolved. `false` means every session on
    /// this install runs on the agent's own identity whatever the client
    /// requests, which the UI needs to say plainly rather than offering a
    /// toggle over nothing.
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified_ms: Option<i64>,
    /// True when `content` holds only the first [`MAX_PREVIEW_BYTES`].
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    /// Why no content is present, when a path resolved but could not be read.
    /// Distinguishes "there is no SOUL.md" from "there is one and it is
    /// unreadable" — different problems with different fixes.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl SoulResponse {
    fn unavailable() -> Self {
        Self {
            available: false,
            path: None,
            content: None,
            bytes: None,
            modified_ms: None,
            truncated: false,
            error: None,
        }
    }
}

/// GET /api/v1/soul
///
/// Authenticated, like every route but `/health`: this returns an absolute
/// filesystem path and the full text of a personal file.
///
/// A path that resolved but cannot be read is a 200 carrying `error`, not a 5xx.
/// The settings panel needs to render the toggle either way, and the reason a
/// file could not be read belongs beside that toggle — a failed request would
/// only be able to say the request failed.
pub async fn get(State(state): State<AppState>) -> Result<Json<SoulResponse>> {
    let Some(path) = state.config.soul_path.as_ref() else {
        return Ok(Json(SoulResponse::unavailable()));
    };

    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        // Resolved at startup, deleted since. Reporting it as absent is right:
        // it is absent, and the runner would reach the same conclusion.
        Err(_) => return Ok(Json(SoulResponse::unavailable())),
    };
    let bytes = metadata.len();
    let modified_ms = paths::modified_ms(&metadata);

    let (content, truncated, error) = match tokio::fs::read(path).await {
        Ok(raw) => {
            let truncated = raw.len() > MAX_PREVIEW_BYTES;
            // Truncate on a char boundary: slicing bytes could split a multi-byte
            // sequence, and `from_utf8_lossy` would render the tail as U+FFFD —
            // a visible corruption in what is meant to be a faithful preview.
            let text = String::from_utf8_lossy(&raw).into_owned();
            let text = if truncated {
                let mut end = MAX_PREVIEW_BYTES.min(text.len());
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                text[..end].to_string()
            } else {
                text
            };
            (Some(text), truncated, None)
        }
        Err(error) => (
            None,
            false,
            Some(format!("Could not read SOUL.md: {}", error.kind())),
        ),
    };

    Ok(Json(SoulResponse {
        available: true,
        path: Some(path.display().to_string()),
        content,
        bytes: Some(bytes),
        modified_ms: Some(modified_ms),
        truncated,
        error,
    }))
}
