//! Deriving a session's label from what the transcript actually says.
//!
//! # Why this lives beside the session store
//!
//! These four rules arrived with the separate archive's client and are **not
//! archive work**. They read a `user` event's text and answer "what would a person
//! call this session", which is a question about a transcript rather than about any
//! particular store — so they outlive the store that first needed them
//! (`docs/database-schema.md` § Search lives beside `history.db`). Their callers are
//! all in the journal: `crate::db::JournalStore::hydrate_session_title`,
//! [`crate::db::JournalStore::set_session_title_if_empty`], and the adopt path in
//! `api::sessions`.
//!
//! **Nothing here translates an id.** Identity is one space throughout
//! (`docs/architecture.md` § Identity), so a helper bridging two of them has nothing to
//! bridge. Ancestry is not here either: a `Lineage` classifier used to re-derive from a
//! parent id and a fork origin, and an adapter now reports the relation typed beside the
//! pointer (`sylph_protocol::NativeSummary::parent_relation`), because the *key* it read
//! is what distinguishes a delegation from a fork and the resulting id cannot.

/// True when a user event is a harness-injected envelope rather than something the
/// person actually typed.
///
/// Agents prepend their own context to the transcript as pseudo user turns:
/// `<permissions instructions>`, `<environment_context>`, `<skills_instructions>`,
/// `<command-name>`, memory files as `# AGENTS.md instructions`, and claude's
/// `Caveat:` preamble. Taking the first user event verbatim therefore labels most
/// sessions with boilerplate that is byte-identical across every session of that
/// agent, which is worse than no label at all.
///
/// *Measured over 311 archived sessions in six real project directories:* skipping
/// these envelopes raises the share of recognizably-labelled sessions from roughly
/// 10% to 98%.
///
/// **Deliberately narrow.** An earlier version also treated a leading `You are ` as
/// boilerplate, which discarded 44 genuine labels in one directory alone — subagent
/// task briefs legitimately open that way, and they are the real ask for those
/// sessions. Only add a rule here for text an agent injects, never for prose a
/// caller might have written.
pub fn is_injected_envelope(text: &str) -> bool {
    let trimmed = text.trim_start();

    // Every agent-injected block observed is XML-ish.
    if trimmed.starts_with('<') {
        return true;
    }

    // Memory and instruction files, injected as a Markdown heading.
    if trimmed.starts_with('#') {
        let first_line = trimmed
            .lines()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if first_line.contains("instructions") {
            return true;
        }
    }

    // claude's preamble for messages generated while running a slash command.
    trimmed.starts_with("Caveat:")
}

/// Known native harness context is retained as raw data, but is not a person's ask.
/// This is narrower than title selection: XML and Markdown can be legitimate prompts.
pub fn is_native_context(text: &str) -> bool {
    let text = text.trim_start();
    [
        "# AGENTS.md instructions",
        "<environment_context>",
        "<permissions instructions>",
        "<skills_instructions>",
        "<codex_internal_context ",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

/// Reduce a multi-line ask to one capped line, for use as a title or list label.
///
/// Every caller renders into a single-line row, so a raw ask would carry embedded
/// newlines into the UI. The first non-empty, non-envelope line is the summary:
/// asks open with their subject and elaborate afterwards.
///
/// Envelope skipping matters here as well as in the caller, because an agent
/// sometimes stitches an injected block onto the front of a real ask within a single
/// event — the event as a whole is not boilerplate, but its first line is.
pub fn summarize_line(ask: &str) -> String {
    /// Long enough to be recognizable, short enough for a 240px sidebar row.
    const MAX_CHARS: usize = 80;

    let line = ask
        .lines()
        .find(|line| !line.trim().is_empty() && !is_injected_envelope(line))
        // Nothing but envelopes: prefer showing them over an empty label.
        .or_else(|| ask.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or(ask)
        .trim();

    // Counting characters, not bytes: these asks are frequently not ASCII, and
    // slicing on a byte index would panic mid-codepoint.
    if line.chars().count() <= MAX_CHARS {
        return line.to_string();
    }
    let truncated: String = line.chars().take(MAX_CHARS).collect();
    format!("{}…", truncated.trim_end())
}

/// A task description recovered from a harness-injected envelope's `summary`.
///
/// Some sessions are delegated work whose every user turn is a
/// `<teammate-message … summary="Author insects pests unit">` block. There is no
/// typed ask to fall back to, but the `summary` attribute is human-written and
/// describes the task, so it is a better label than nothing.
///
/// Hand-parsed rather than pulled through a regex dependency: the pattern is a
/// quoted attribute value, and the surrounding text is untrusted transcript content
/// where a cheap scan is easier to reason about than a pattern. XML entities are
/// decoded because the text escapes them (`&quot;`, `&apos;`), and a label reading
/// `meadow&apos;s asks` is worse than one reading `meadow's asks`.
pub fn envelope_summary(text: &str) -> Option<String> {
    // Only inside an opening tag, so prose that merely contains `summary="` in a
    // sentence cannot masquerade as one.
    let tag_end = text.find('>')?;
    let tag = &text[..tag_end];

    let start = tag.find("summary=\"")? + "summary=\"".len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    let raw = rest[..end].trim();
    if raw.is_empty() {
        return None;
    }

    let decoded = raw
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        // Ampersand last, so a decoded entity cannot be decoded twice.
        .replace("&amp;", "&");

    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The rule that raised recognizable labels from ~10% to 98%.
    #[test]
    fn an_injected_envelope_is_not_the_users_ask() {
        assert!(is_injected_envelope("<permissions instructions> …"));
        assert!(is_injected_envelope("<environment_context>"));
        assert!(is_injected_envelope("# AGENTS.md instructions"));
        assert!(is_injected_envelope("Caveat: this message was generated…"));
    }

    /// The narrow-scope warning, pinned. An earlier version treated a leading
    /// `You are ` as boilerplate and discarded 44 genuine labels, because subagent
    /// task briefs legitimately open that way.
    #[test]
    fn a_subagent_brief_is_a_real_ask_and_not_boilerplate() {
        assert!(!is_injected_envelope("You are reviewing Task 4."));
        assert!(!is_injected_envelope("Fix the login bug"));
        // A heading that is not an instruction file is prose.
        assert!(!is_injected_envelope("# Design notes"));
    }

    #[test]
    fn a_summary_is_the_first_line_that_is_not_an_envelope() {
        let ask = "<permissions instructions> Filesystem sandboxing defines which files\n\
                   Fix the SSE reconnect loop";
        assert_eq!(summarize_line(ask), "Fix the SSE reconnect loop");
    }

    /// Truncation counts characters, not bytes. These asks are frequently not
    /// ASCII, and slicing on a byte index would panic mid-codepoint.
    #[test]
    fn a_long_ask_is_truncated_on_a_character_boundary() {
        let ask = "请帮我".repeat(40);
        let summary = summarize_line(&ask);
        assert!(summary.ends_with('…'));
        assert_eq!(summary.chars().count(), 81, "80 chars plus the ellipsis");
    }

    /// Slash-command sessions carry nothing else, and a blank row is worse than
    /// showing the envelope.
    #[test]
    fn an_envelope_only_ask_still_yields_a_label() {
        assert_eq!(
            summarize_line("<command-name>/theme</command-name>"),
            "<command-name>/theme</command-name>"
        );
    }

    #[test]
    fn a_summary_attribute_is_recovered_and_unescaped() {
        assert_eq!(
            envelope_summary(r#"<teammate-message summary="meadow&apos;s asks">body"#),
            Some("meadow's asks".to_string())
        );
        // Prose merely containing the token is not a tag.
        assert_eq!(envelope_summary("we set summary=\"x\" somewhere"), None);
        assert_eq!(envelope_summary("<teammate-message>"), None);
    }
}
