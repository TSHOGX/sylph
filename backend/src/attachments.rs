//! Staging uploaded attachments to disk for adapters to read.
//!
//! A browser sends attachments as base64. `sylph.agent.v3` carries them as
//! absolute paths in the `send` frame's `attachments[]`, so they are decoded and
//! written under `{data_dir}/attachments/{session}/{staging-id}/` before the
//! frame goes out.
//!
//! Paths rather than bytes because the adapter is a separate process and each one
//! wants the file in a different shape: `claude` inlines images as base64 content
//! blocks — the Read tool is unreliable on binary image data — and names PDFs and
//! text by path so the CLI's own tools handle them. Bytes on the wire would make
//! every adapter write the file back out to get a path anyway, on a protocol whose
//! frame is one JSON line.
//!
//! They are staged under the data dir rather than a temporary directory because a
//! turn is durable and its input has to outlive the request that posted it.

use std::path::{Path, PathBuf};

use base64::Engine;
use sylph_protocol::SessionId;

use crate::error::{Error, Result};
use crate::models::AttachmentData;

/// Per-turn limits from `docs/api-design.md`.
const MAX_ATTACHMENTS: usize = 5;
/// Per-file ceiling, shared with the workspace upload endpoint so a browser
/// upload has one limit however it reaches disk.
pub const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 10 * 1024 * 1024;

/// A session's attachment directory name.
///
/// # Why the encoded triple cannot be used verbatim
///
/// A session's id contains a `native_id`, which is the **vendor's own string** and
/// deliberately unvalidated (`sylph_protocol::NativeId::new` is infallible: refusing an
/// id for containing a character Sylph finds inconvenient would make a real session
/// unrepresentable). Measured, `claude` names a delegate
/// `<parent>/agent-<name>-<hash>` — so a native id contains `/` in practice, not just in
/// principle.
///
/// Joined onto a path unchanged, that id is a **directory traversal**: `..` would climb
/// out of the data directory, and a leading `/` would make `Path::join` discard
/// everything before it and treat the id as absolute. This is the same class of hole
/// `AGENTS.md` records twice for the file endpoints, arriving through a component nobody
/// thought of as user input.
///
/// # The encoding, and what was rejected
///
/// Every byte outside `[A-Za-z0-9._-]` becomes `%XX`. Percent-encoding rather than the
/// alternatives, for reasons that are about failure modes rather than aesthetics:
///
/// - **Replacing unsafe characters with `_`** is not injective: `a/b` and `a_b` would
///   collide, so two sessions would share a staging directory and one turn's
///   attachments would be visible to another session's agent.
/// - **Hashing the id** is injective enough but not reversible, so a human debugging a
///   staging directory cannot tell which session it belongs to — and these files
///   outlive the request that posted them by design.
///
/// `.` and `-` are kept unescaped because a native id is usually a UUID and escaping its
/// hyphens would make every directory name unreadable for no gain. A bare `.` or `..`
/// cannot result: the id is prefixed by the environment and agent, both of which are
/// non-empty and neither of which may contain the delimiter.
fn session_dir_name(session_id: &SessionId) -> String {
    let encoded = session_id.encode();
    let mut out = String::with_capacity(encoded.len());
    for byte in encoded.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Decode and write attachments, returning the staged absolute paths.
pub async fn stage(
    data_dir: &Path,
    session_id: &SessionId,
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

    // `turn_id` is server-minted (a UUID) and needs no such treatment; the session
    // component is the only one carrying a vendor's string.
    let dir = data_dir
        .join("attachments")
        .join(session_dir_name(session_id))
        .join(turn_id);
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

    mod session_dir {
        use super::super::session_dir_name;
        use sylph_protocol::{AgentName, EnvironmentId, NativeId, SessionId};

        fn id(native: &str) -> SessionId {
            SessionId::new(
                EnvironmentId::parse("env7fk2").unwrap(),
                AgentName::parse("claude").unwrap(),
                NativeId::new(native),
            )
        }

        /// The ordinary case stays legible, which is the whole reason this is not a
        /// hash: a person debugging a staging directory has to be able to tell which
        /// session it belongs to.
        #[test]
        fn a_uuid_native_id_stays_readable() {
            assert_eq!(
                session_dir_name(&id("9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60")),
                "env7fk2%3Aclaude%3A9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60"
            );
        }

        /// A delegate's native id contains a slash — measured on `claude`, which names
        /// one `<parent>/agent-<name>`. Joined onto a path unchanged this would create a
        /// nested directory, and the traversal cases below are the same defect with
        /// teeth.
        #[test]
        fn a_slash_in_a_native_id_cannot_create_a_subdirectory() {
            let name = session_dir_name(&id("9bdacb6e/agent-explore"));
            assert!(!name.contains('/'), "{name}");
            assert!(name.ends_with("9bdacb6e%2Fagent-explore"), "{name}");
        }

        /// The reason this function exists rather than the id being used verbatim.
        #[test]
        fn traversal_and_absolute_ids_are_neutralized() {
            for hostile in ["../../etc/passwd", "/etc/passwd", ".."] {
                let name = session_dir_name(&id(hostile));
                assert!(!name.contains('/'), "{name}");
                assert_ne!(name, "..");
                assert!(
                    !std::path::Path::new(&name).is_absolute(),
                    "a staged path must stay under the data directory: {name}"
                );
                // One component, so `join` cannot climb.
                assert_eq!(std::path::Path::new(&name).components().count(), 1, "{name}");
            }
        }

        /// Injective, which `_`-replacement would not be: two sessions sharing a
        /// staging directory would show one turn's attachments to another session's
        /// agent.
        #[test]
        fn two_ids_differing_only_in_punctuation_do_not_collide() {
            assert_ne!(session_dir_name(&id("a/b")), session_dir_name(&id("a_b")));
            assert_ne!(session_dir_name(&id("a b")), session_dir_name(&id("a-b")));
        }
    }
}
