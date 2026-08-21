//! Staging uploaded attachments to disk for runners to read.
//!
//! The runner protocol passes attachments as absolute paths
//! (`--attachment ABS_PATH`), not inline bytes, so base64 payloads from the
//! browser are decoded and written under
//! `{data_dir}/attachments/{session}/{turn}/` before the runner is spawned.

use std::path::{Path, PathBuf};

use base64::Engine;

use crate::error::{Error, Result};
use crate::models::AttachmentData;

/// Per-turn limits from `docs/api-design.md`.
const MAX_ATTACHMENTS: usize = 5;
/// Per-file ceiling, shared with the workspace upload endpoint so a browser
/// upload has one limit however it reaches disk.
pub const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 10 * 1024 * 1024;

/// Decode and write attachments, returning the staged absolute paths.
pub async fn stage(
    data_dir: &Path,
    session_id: &str,
    turn_id: &str,
    attachments: &[AttachmentData],
) -> Result<Vec<PathBuf>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(Error::AttachmentInvalid(format!(
            "Too many attachments: {} (max {})",
            attachments.len(),
            MAX_ATTACHMENTS
        )));
    }

    let dir = data_dir.join("attachments").join(session_id).join(turn_id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| Error::Internal(format!("Failed to create attachment dir: {}", e)))?;

    let mut staged = Vec::with_capacity(attachments.len());
    let mut total = 0usize;

    for attachment in attachments {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(attachment.data.as_bytes())
            .map_err(|e| {
                Error::AttachmentInvalid(format!("Invalid base64 in '{}': {}", attachment.name, e))
            })?;

        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(Error::AttachmentInvalid(format!(
                "Attachment '{}' is {} bytes (max {})",
                attachment.name,
                bytes.len(),
                MAX_ATTACHMENT_BYTES
            )));
        }
        total += bytes.len();
        if total > MAX_TOTAL_BYTES {
            return Err(Error::AttachmentInvalid(format!(
                "Attachments total {} bytes (max {})",
                total, MAX_TOTAL_BYTES
            )));
        }

        let path = dir.join(safe_filename(&attachment.name, staged.len()));
        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|e| Error::Internal(format!("Failed to write attachment: {}", e)))?;
        staged.push(path);
    }

    Ok(staged)
}

/// Reduce a client-supplied filename to a single safe path component.
///
/// The name reaches us from the browser and is used to build a filesystem path,
/// so directory separators and `..` must not survive. Only the final component
/// is kept, and anything outside a conservative allowlist becomes dropped. An
/// index prefix keeps two attachments with the same name from colliding.
fn safe_filename(name: &str, index: usize) -> String {
    format!("{}-{}", index, sanitize_filename(name))
}

/// Reduce a client-supplied filename to one safe path component, unprefixed.
///
/// Split out from [`safe_filename`] because a workspace upload keeps the user's
/// own filename — an index prefix is right for per-turn staging, where collisions
/// between two attachments in one message are the hazard, and wrong for a file
/// the user is placing in their own project by name.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("attachment").trim();

    let cleaned: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
        .collect();

    let cleaned = cleaned.trim_matches(['.', ' ']).to_string();
    let cleaned = if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned
    };

    // Leave room for a prefix while staying well under any filesystem limit.
    cleaned.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::safe_filename;

    #[test]
    fn strips_directory_traversal() {
        assert_eq!(safe_filename("../../etc/passwd", 0), "0-passwd");
        assert_eq!(safe_filename("/absolute/path.png", 1), "1-path.png");
        assert_eq!(safe_filename("..", 2), "2-attachment");
    }

    #[test]
    fn keeps_ordinary_names_readable() {
        assert_eq!(safe_filename("screenshot.png", 0), "0-screenshot.png");
        assert_eq!(safe_filename("my report v2.pdf", 3), "3-my report v2.pdf");
    }

    #[test]
    fn replaces_unsafe_characters() {
        assert_eq!(safe_filename("a;rm -rf$(x).txt", 0), "0-arm -rfx.txt");
        assert_eq!(safe_filename("", 0), "0-attachment");
    }
}
