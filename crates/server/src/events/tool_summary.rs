//! Tool semantics for both materialized counters and browser headers. Vendor
//! payloads stay lossless; this bounded description is derived before previewing.
use super::StreamEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSummary {
    pub kind: String,
    pub name: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

fn clip(value: &str, bytes: usize) -> String {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn first(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        input
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(|s| clip(s, 120))
    })
}

pub fn describe(event: &StreamEvent) -> ToolSummary {
    let name = event
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("tool");
    let normalized: String = name
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    // Tool names belong to vendors and installed MCP servers. These are semantic
    // families, not aliases for obsolete Sylph spellings. Specific edits win first.
    let rules: &[(&str, &[&str])] = &[
        (
            "edit",
            &["multiedit", "strreplace", "applypatch", "edit", "patch"],
        ),
        ("write", &["writefile", "createfile", "write", "create"]),
        ("read", &["readfile", "read", "cat", "view", "notebookread"]),
        (
            "shell",
            &["bash", "shell", "exec", "terminal", "command", "run"],
        ),
        (
            "search",
            &["grep", "glob", "search", "find", "ripgrep", "ls", "list"],
        ),
        ("task", &["task", "agent", "todo", "plan", "delegate"]),
    ];
    let kind = rules
        .iter()
        .find(|(_, patterns)| patterns.iter().any(|pattern| normalized.contains(pattern)))
        .map_or("other", |(kind, _)| *kind);
    // Claude and Pi use data.input. Codex live commandExecution carries a
    // top-level command and plain command text; native Codex calls carry input.
    // A non-JSON string input is retained as the vendor's whole argument.
    let input = match event.data.as_ref().and_then(|data| data.get("input")) {
        Some(Value::Object(input)) => Value::Object(input.clone()),
        Some(Value::String(raw)) => {
            serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({"command":raw}))
        }
        _ => event
            .data
            .as_ref()
            .filter(|data| data.get("command").is_some())
            .cloned()
            .or_else(|| serde_json::from_str(&event.text).ok())
            .unwrap_or(Value::Null),
    };
    let path = first(
        &input,
        &[
            "file_path",
            "filePath",
            "path",
            "notebook_path",
            "notebookPath",
            "target_file",
            "targetFile",
            "filename",
            "file",
        ],
    );
    let command = first(&input, &["command", "cmd", "script", "shell_command"])
        .or_else(|| (name == "exec" && !event.text.is_empty()).then(|| clip(&event.text, 120)));
    let pattern = first(&input, &["pattern", "query", "regex", "glob", "search"]);
    let summary = command
        .clone()
        .or_else(|| {
            pattern.as_ref().map(|pattern| match &path {
                Some(path) => format!("{pattern} in {path}"),
                None => pattern.clone(),
            })
        })
        .or_else(|| path.clone())
        .or_else(|| {
            input.as_object().and_then(|object| {
                object.iter().find_map(|(key, value)| {
                    value
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| format!("{}: {}", clip(key, 40), clip(s, 120)))
                })
            })
        })
        .unwrap_or_else(|| clip(name, 120));
    ToolSummary {
        kind: kind.into(),
        name: clip(name, 256),
        summary: clip(&summary, 160),
        path,
        command,
        pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_semantics_cover_vendor_names_arguments_and_partial_inputs() {
        let mut event: StreamEvent = serde_json::from_value(json!({"seq":1,"kind":"tool_call","stability":"stable","source":"agent","arrivalAtMs":1,"createdAtMs":1})).unwrap();
        for (name, expected) in [
            ("Read", "read"),
            ("mcp__fs__read_file", "read"),
            ("MultiEdit", "edit"),
            ("str_replace_editor", "edit"),
            ("Write", "write"),
            ("exec", "shell"),
            ("Bash", "shell"),
            ("Grep", "search"),
            ("Glob", "search"),
            ("Task", "task"),
            ("SomethingNovel", "other"),
        ] {
            event.name = Some(name.into());
            assert_eq!(describe(&event).kind, expected, "{name}");
        }
        for data in [
            json!({"input":{"command":"ls -la"}}),
            json!({"input":"ls -la"}),
            json!({"command":"ls -la"}),
        ] {
            event.data = Some(data);
            assert_eq!(describe(&event).command.as_deref(), Some("ls -la"));
        }
        event.name = Some("Read".into());
        event.data = None;
        event.text = r#"{"file_path":"src/lib.rs"}"#.into();
        assert_eq!(describe(&event).path.as_deref(), Some("src/lib.rs"));
        event.text = r#"{"file_pa"#.into();
        assert!(describe(&event).path.is_none());
        assert_eq!(describe(&event).summary, "Read");
        event.data = Some(json!({"input":{"subject":"the thing"}}));
        assert_eq!(describe(&event).summary, "subject: the thing");
    }
}
