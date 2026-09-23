//! Host annotations describe source content independently of protocol roles.
//! See `docs/unified-trace-codex.md` § Native content annotations.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct Classification {
    pub purpose: MessagePurpose,
    pub harness: bool,
}
pub(super) fn classify(value: &Value) -> Option<Classification> {
    if matches!(string(value, "type"), Some("userMessage" | "UserMessage")) {
        return Some(Classification {
            purpose: MessagePurpose::TaskInput,
            harness: false,
        });
    }
    let kinds = value
        .get("internal_chat_message_metadata_passthrough")?
        .get("content_item_kinds")?
        .as_array()?;
    let parts = value.get("content")?.as_array()?;
    if kinds.is_empty()
        || kinds.len() != parts.len()
        || parts.iter().any(|part| {
            string(part, "type") != Some("input_text") || string(part, "text").is_none()
        })
    {
        return None;
    }
    let mut classified = kinds.iter().map(|kind| classification(kind.as_str()?));
    let mut combined = classified.next()??;
    for next in classified {
        let next = next?;
        combined.harness &= next.harness;
        if combined.purpose != next.purpose {
            combined.purpose = MessagePurpose::Unclassified;
        }
    }
    Some(combined)
}
fn classification(kind: &str) -> Option<Classification> {
    let (purpose, harness) = match kind {
        "user.text" => (MessagePurpose::TaskInput, false),
        "environments.environment_context" => (MessagePurpose::Context, true),
        "generic.turn_aborted"
        | "multi_agent.subagent_notification"
        | "apply_patch.legacy_exec_command_warning"
        | "model_switch.legacy_mismatch_warning"
        | "unified_exec.legacy_process_limit_warning"
        | "guardian.context_omission" => (MessagePurpose::Notice, true),
        // The pinned InternalModelContextFragment constructs this kind from a
        // validated source label. These are annotation values, never text markers.
        // A quoted goal wrapper remains ordinary input when labeled user.text.
        _ if kind
            .strip_suffix(".internal_context")
            .is_some_and(valid_internal_source) =>
        {
            (MessagePurpose::Context, true)
        }
        _ => return None,
    };
    Some(Classification { purpose, harness })
}
fn valid_internal_source(source: &str) -> bool {
    let mut chars = source.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}
