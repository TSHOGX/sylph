//! Attachment filename safety shared by workspace uploads and remote input metadata.

pub const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

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

    // Leave room for a prefix while staying well under any filesystem limit.
    cleaned.chars().take(120).collect()
}
