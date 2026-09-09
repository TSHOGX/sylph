//! Staging a turn's attachments on the machine whose adapter reads them.
//!
//! A browser posts attachments as base64 and the server forwards the bytes unchanged
//! (`sylph_protocol::WireAttachment`). An adapter wants a *path*: `claude` inlines
//! images as content blocks and names PDFs and text by path so the CLI's own tools
//! handle them, and `pi` reads images off disk. So the bytes are written here, under
//! the envagent's own root, and the adapter is handed local paths.
//!
//! Staged under the envagent's root rather than a temporary directory because a
//! turn's input has to outlive the request that posted it: the adapter reads a file
//! when the vendor gets to it, which may be seconds after the command returned.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use base64::Engine;
use sylph_protocol::WireAttachment;

/// Per-turn limits, from `docs/api/files.md`.
///
/// Checked here as well as by the server, because the server's check protects its
/// own memory and this one protects this machine's disk. The two are the same
/// numbers on purpose and a mismatch would refuse on one side what the other let
/// through.
pub const MAX_ATTACHMENTS: usize = 5;
pub const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 10 * 1024 * 1024;

/// Decode and write a turn's attachments, returning the staged absolute paths.
///
/// `scope` is the session or draft the turn belongs to, already reduced to one path
/// component by `scope_dir_name`; `turn_id` is server-minted and safe as given.
pub async fn stage(
    root: &Path,
    scope: &str,
    turn_id: &str,
    attachments: &[WireAttachment],
) -> Result<Vec<String>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(anyhow!(
            "too many attachments: {} (max {MAX_ATTACHMENTS})",
            attachments.len()
        ));
    }
    let dir = root
        .join(scope_dir_name(scope))
        .join(scope_dir_name(turn_id));
    tokio::fs::create_dir_all(&dir).await?;

    let mut staged = Vec::with_capacity(attachments.len());
    let mut total = 0usize;
    for (index, attachment) in attachments.iter().enumerate() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(attachment.data.as_bytes())
            .map_err(|error| anyhow!("invalid base64 in '{}': {error}", attachment.name))?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(anyhow!(
                "attachment '{}' is {} bytes (max {MAX_ATTACHMENT_BYTES})",
                attachment.name,
                bytes.len()
            ));
        }
        total += bytes.len();
        if total > MAX_TOTAL_BYTES {
            return Err(anyhow!(
                "attachments total {total} bytes (max {MAX_TOTAL_BYTES})"
            ));
        }
        let path = dir.join(format!("{index}-{}", sanitize_filename(&attachment.name)));
        tokio::fs::write(&path, &bytes).await?;
        staged.push(path.display().to_string());
    }
    Ok(staged)
}

/// Remove everything staged for one scope, when a draft is discarded or a session
/// closes.
pub async fn cleanup(root: &Path, scope: &str) {
    let dir = root.join(scope_dir_name(scope));
    if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                "could not clean attachment directory {}: {error}",
                dir.display()
            );
        }
    }
}

/// Percent-encode an id before using it as a path component.
///
/// A session id contains a `native_id`, which is the **vendor's own string** and
/// deliberately unvalidated; measured, `claude` names a delegate
/// `<parent>/agent-<name>-<hash>`, so a native id contains `/` in practice. Joined onto a
/// path unchanged that is a directory traversal. Percent-encoding rather than
/// replacing is injective, so two ids cannot share a directory, and it stays readable
/// for a person debugging a staging directory.
fn scope_dir_name(scope: &str) -> String {
    let mut out = String::with_capacity(scope.len());
    for byte in scope.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    if out.is_empty() || out == "." || out == ".." {
        "scope".to_string()
    } else {
        out
    }
}

/// Reduce a client-supplied filename to one safe path component.
///
/// Only the final component survives, and anything outside a conservative allowlist
/// is dropped: the name reaches this machine from a browser and is used to build a
/// filesystem path, so separators and `..` must not survive.
pub fn sanitize_filename(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("attachment")
        .trim();
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
    cleaned.chars().take(120).collect()
}

/// Where staged attachments live under an envagent root.
pub fn attachments_dir(env_home: &Path) -> PathBuf {
    env_home.join("attachments")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_in_an_id_cannot_create_a_subdirectory() {
        assert!(!scope_dir_name("env:claude:a/b").contains('/'));
        assert_ne!(scope_dir_name("a/b"), scope_dir_name("a_b"));
        assert_eq!(scope_dir_name(".."), "scope");
    }

    #[test]
    fn filenames_lose_traversal_and_separators() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("a/b\\c.txt"), "c.txt");
        assert_eq!(sanitize_filename("..."), "attachment");
        assert_eq!(
            sanitize_filename("报告.pdf"),
            ".pdf".trim_matches('.').to_string() + ""
        );
    }

    #[tokio::test]
    async fn staged_bytes_are_the_decoded_payload_and_stay_under_the_scope() {
        let dir = tempfile::tempdir().unwrap();
        let staged = stage(
            dir.path(),
            "env7fk2:mock:n/1",
            "turn-1",
            &[WireAttachment {
                name: "../note.txt".into(),
                mime_type: "text/plain".into(),
                data: "aGVsbG8=".into(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(staged.len(), 1);
        assert!(staged[0].starts_with(dir.path().to_str().unwrap()));
        assert_eq!(std::fs::read_to_string(&staged[0]).unwrap(), "hello");
        cleanup(dir.path(), "env7fk2:mock:n/1").await;
        assert!(!Path::new(&staged[0]).exists());
    }

    #[tokio::test]
    async fn malformed_base64_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let error = stage(
            dir.path(),
            "s",
            "t",
            &[WireAttachment {
                name: "x".into(),
                mime_type: "text/plain".into(),
                data: "not base64!!".into(),
            }],
        )
        .await
        .unwrap_err();
        assert!(format!("{error}").contains("base64"));
    }
}
