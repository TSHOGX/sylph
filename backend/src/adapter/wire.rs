//! The `sylph.agent.v2` wire format.
//!
//! NDJSON in both directions on a held-open duplex stream. See
//! `docs/session-protocol.md`; this module is that document as types.
//!
//! Two rules shape every declaration here, and both are what let an old server
//! talk to a new adapter:
//!
//! * **Unknown message types are ignored, never fatal.** So the outer envelope
//!   deserializes `t` first and the payload second, rather than as one closed
//!   enum that would reject a line it merely does not recognize.
//! * **Unknown fields are tolerated.** No `deny_unknown_fields` anywhere.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The protocol version string exchanged in the handshake.
pub const PROTOCOL: &str = "sylph.agent.v2";

/// What this server can be asked to do by an adapter.
///
/// Sent in `hello` so an adapter cannot raise a request kind nobody will answer.
/// This is the one capability mismatch that would hang a session rather than
/// degrade it: an agent blocked on an approval that no UI will ever render waits
/// forever, so it is negotiated up front instead of discovered at raise time.
pub const SERVER_ACCEPTS: &[&str] = &["approval", "dialog", "elicitation", "choice"];

/// Something an adapter says it can do. A closed vocabulary: unknown entries are
/// ignored rather than rejected, so a newer adapter degrades to its known subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Can end a turn without ending the session.
    Interrupt,
    /// Accepts text mid-turn.
    Steer,
    /// Raises approval requests.
    Approval,
    /// Reports subagent trajectories.
    Subagent,
    /// Reports a live slash-command list.
    Slash,
    /// Accepts a model change on a live session.
    SetModel,
    /// Answers context-usage queries.
    Context,
    /// Can fork a session.
    Fork,
}

/// A flag an adapter declares, carried unchanged from the v1 `--describe`
/// schema so an adapter declares its options in one place rather than two.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagSpec {
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// A slash command the agent itself reports.
///
/// The point of the redesign in one struct: on a real project the handshake
/// returns dozens of these — skills, project commands, plugins — where v1's
/// `--describe` returned the one or two an adapter author hand-wrote.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Free-form hint for what follows the command, e.g. `[target]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_hint: Option<String>,
    /// Where it came from: `skill`, `project`, `plugin`, `builtin`. Advisory,
    /// used only for grouping in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// A model the agent offers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: String,
    #[serde(default)]
    pub label: String,
}

/// A subagent definition the agent can delegate to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// The live capability surface of an open session.
///
/// Re-sent as a `capabilities` event whenever it changes, rather than polled: a
/// skill edited on disk or a plugin loading changes what the composer should
/// offer, and a list the client refreshes on its own schedule is a list that is
/// wrong for however long that schedule is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub subagents: Vec<SubagentSpec>,
    #[serde(default)]
    pub models: Vec<ModelSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
}

/// An option offered on an adapter request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestOption {
    pub id: String,
    pub label: String,
    /// Whether choosing this is a lasting decision rather than a one-off, so a
    /// UI can mark "allow always" differently from "allow once".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub persistent: bool,
}

/// A question the agent needs a human to answer.
///
/// The inversion v1 could not express. Journaled rather than merely published,
/// because a pending request is durable state: a client closing its laptop must
/// not strand an agent forever.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterRequest {
    /// `approval`, `dialog`, `elicitation`, `choice`.
    pub kind: String,
    /// The agent's own id for the tool call this concerns, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default)]
    pub title: String,
    /// What is actually being asked, shaped by `kind`. For an approval this is
    /// the proposed tool input, which is also what an editing client sends back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Vec<RequestOption>,
    /// Which option applies if the user declines to choose. Deliberately not
    /// auto-applied by the server: a timeout that silently approves would remove
    /// the user from the decision the protocol exists to give them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// `safe`, `destructive`, `network`. Advisory, for emphasis in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
}

/// A line the adapter sent us.
///
/// `t` is read as a string rather than matched by an untagged enum so an
/// unrecognized type reaches [`Inbound::Unknown`] and is logged, instead of
/// failing the whole line and taking the link down with it.
#[derive(Debug, Clone)]
pub enum Inbound {
    /// A reply to one of our commands.
    Ok { id: String, body: serde_json::Value },
    /// A refusal of one of our commands.
    Err { id: String, code: String, message: String },
    /// The adapter is asking us something and is waiting.
    Request { id: String, request: AdapterRequest },
    /// A normalized event. Payload stays `Value` because the event vocabulary is
    /// additive by design and this module must not be the thing that has to
    /// learn a new kind.
    Event { body: serde_json::Value },
    /// A recognized type we have no handling for, or one we do not know at all.
    Unknown { t: String },
}

/// Parse one inbound NDJSON line.
///
/// Returns `None` for a line that is not JSON at all — adapters may write
/// diagnostics to stdout and the protocol tolerates it.
pub fn parse_inbound(line: &str) -> Option<Inbound> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let t = value.get("t")?.as_str()?.to_string();
    let id = value
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(match t.as_str() {
        "ok" => Inbound::Ok {
            id: id?,
            body: value,
        },
        "err" => Inbound::Err {
            id: id?,
            code: value
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("ADAPTER_ERROR")
                .to_string(),
            message: value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "request" => {
            let request = serde_json::from_value::<AdapterRequest>(value.clone()).ok()?;
            Inbound::Request { id: id?, request }
        }
        "event" => Inbound::Event { body: value },
        _ => Inbound::Unknown { t },
    })
}

/// A command we send to the adapter.
///
/// Serialized by hand rather than derived, because `t` and `id` sit beside the
/// payload's own fields at the top level of one flat object. A derived enum
/// would nest them.
#[derive(Debug, Clone)]
pub enum Outbound {
    Hello,
    Open {
        workdir: String,
        session_id: Option<String>,
        /// A path, or `""` for no injection. The empty string is meaningful:
        /// it is how the native-session invariant is enforced server-side.
        soul: Option<String>,
        flags: HashMap<String, String>,
        permission_mode: Option<String>,
        env: HashMap<String, String>,
    },
    Send {
        text: String,
        attachments: Vec<String>,
        command: Option<String>,
        args: HashMap<String, String>,
    },
    Interrupt,
    Steer {
        text: String,
    },
    Set {
        field: String,
        value: serde_json::Value,
    },
    /// Answer an adapter request. `id` echoes the request's id, so this is the
    /// one outbound message whose id is not ours to choose.
    Resolve {
        request_id: String,
        option: String,
        input: Option<serde_json::Value>,
    },
    Query {
        what: String,
    },
    Close,
}

impl Outbound {
    /// Render as one NDJSON line, including the trailing newline.
    ///
    /// `id` is supplied by the caller so correlation stays with whoever tracks
    /// pending commands, rather than being minted here where it could collide.
    pub fn to_line(&self, id: &str) -> String {
        let mut object = serde_json::Map::new();
        object.insert("id".into(), serde_json::Value::String(id.to_string()));

        let t = match self {
            Self::Hello => {
                object.insert("protocol".into(), PROTOCOL.into());
                object.insert(
                    "server".into(),
                    format!("sylph-web/{}", env!("CARGO_PKG_VERSION")).into(),
                );
                object.insert("accepts".into(), SERVER_ACCEPTS.into());
                "hello"
            }
            Self::Open {
                workdir,
                session_id,
                soul,
                flags,
                permission_mode,
                env,
            } => {
                object.insert("workdir".into(), workdir.clone().into());
                object.insert(
                    "sessionId".into(),
                    match session_id {
                        Some(sid) => sid.clone().into(),
                        None => serde_json::Value::Null,
                    },
                );
                if let Some(soul) = soul {
                    object.insert("soul".into(), soul.clone().into());
                }
                object.insert("flags".into(), serde_json::to_value(flags).unwrap_or_default());
                if let Some(mode) = permission_mode {
                    object.insert("permissionMode".into(), mode.clone().into());
                }
                object.insert("env".into(), serde_json::to_value(env).unwrap_or_default());
                "open"
            }
            Self::Send {
                text,
                attachments,
                command,
                args,
            } => {
                object.insert("text".into(), text.clone().into());
                if !attachments.is_empty() {
                    object.insert(
                        "attachments".into(),
                        serde_json::to_value(attachments).unwrap_or_default(),
                    );
                }
                if let Some(command) = command {
                    object.insert("command".into(), command.clone().into());
                    object.insert("args".into(), serde_json::to_value(args).unwrap_or_default());
                }
                "send"
            }
            Self::Interrupt => "interrupt",
            Self::Steer { text } => {
                object.insert("text".into(), text.clone().into());
                "steer"
            }
            Self::Set { field, value } => {
                object.insert(field.clone(), value.clone());
                "set"
            }
            Self::Resolve {
                request_id,
                option,
                input,
            } => {
                // Overwrites the caller's id on purpose: a resolve is addressed
                // to the request it answers.
                object.insert("id".into(), request_id.clone().into());
                object.insert("option".into(), option.clone().into());
                if let Some(input) = input {
                    object.insert("input".into(), input.clone());
                }
                "resolve"
            }
            Self::Query { what } => {
                object.insert("what".into(), what.clone().into());
                "query"
            }
            Self::Close => "close",
        };

        object.insert("t".into(), t.into());
        let mut line = serde_json::Value::Object(object).to_string();
        line.push('\n');
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_of(message: &Outbound, id: &str) -> serde_json::Value {
        let line = message.to_line(id);
        assert!(line.ends_with('\n'), "NDJSON needs its terminator");
        assert_eq!(line.matches('\n').count(), 1, "one object per line");
        serde_json::from_str(line.trim()).expect("must be valid JSON")
    }

    #[test]
    fn hello_declares_the_protocol_and_what_this_server_can_be_asked() {
        let value = line_of(&Outbound::Hello, "h1");
        assert_eq!(value["t"], "hello");
        assert_eq!(value["id"], "h1");
        assert_eq!(value["protocol"], PROTOCOL);
        // Advertising approval is what permits an adapter to raise one. An
        // adapter that raises an unadvertised kind would block on a reply no UI
        // produces, which is why this is asserted rather than assumed.
        let accepts = value["accepts"].as_array().unwrap();
        assert!(accepts.iter().any(|a| a == "approval"));
    }

    /// The empty string is a value, not an absence: it is how a native session
    /// tells the adapter to inject nothing. Serializing it as a missing field
    /// would let the adapter fall back to its default and inject an identity
    /// into a preserved transcript.
    #[test]
    fn an_empty_soul_is_sent_as_an_empty_string_not_omitted() {
        let open = Outbound::Open {
            workdir: "/w".into(),
            session_id: None,
            soul: Some(String::new()),
            flags: HashMap::new(),
            permission_mode: None,
            env: HashMap::new(),
        };
        let value = line_of(&open, "o1");
        assert_eq!(value["soul"], "");
        assert!(value.get("soul").is_some());
    }

    #[test]
    fn a_fresh_session_sends_an_explicit_null_rather_than_omitting_the_id() {
        // Explicit null distinguishes "start fresh" from "field forgotten",
        // which is the difference between a new context and a silent resume.
        let open = Outbound::Open {
            workdir: "/w".into(),
            session_id: None,
            soul: None,
            flags: HashMap::new(),
            permission_mode: None,
            env: HashMap::new(),
        };
        let value = line_of(&open, "o1");
        assert!(value["sessionId"].is_null());
        assert!(value.get("soul").is_none(), "an unset soul means resolve your default");
    }

    #[test]
    fn a_command_turn_carries_its_name_and_args_beside_the_text() {
        let send = Outbound::Send {
            text: String::new(),
            attachments: vec![],
            command: Some("verify".into()),
            args: HashMap::from([("target".to_string(), "the auth change".to_string())]),
        };
        let value = line_of(&send, "c9");
        assert_eq!(value["t"], "send");
        assert_eq!(value["command"], "verify");
        assert_eq!(value["args"]["target"], "the auth change");
    }

    #[test]
    fn a_plain_turn_omits_command_and_attachment_fields_entirely() {
        let send = Outbound::Send {
            text: "hello".into(),
            attachments: vec![],
            command: None,
            args: HashMap::new(),
        };
        let value = line_of(&send, "c1");
        assert_eq!(value["text"], "hello");
        assert!(value.get("command").is_none());
        assert!(value.get("attachments").is_none());
    }

    /// A resolve is addressed to the request it answers, so it must carry that
    /// id even though every other outbound message uses the caller's.
    #[test]
    fn a_resolve_is_addressed_to_the_request_not_to_a_new_command() {
        let resolve = Outbound::Resolve {
            request_id: "r4".into(),
            option: "allow".into(),
            input: Some(serde_json::json!({"command": "ls -la"})),
        };
        let value = line_of(&resolve, "ignored-c12");
        assert_eq!(value["id"], "r4");
        assert_eq!(value["option"], "allow");
        // Edited input is how "allow, but change the command" works — a real
        // terminal affordance a yes/no dialog would drop.
        assert_eq!(value["input"]["command"], "ls -la");
    }

    #[test]
    fn an_approval_request_parses_with_its_options_and_proposed_input() {
        let line = r#"{"t":"request","id":"r4","kind":"approval","toolUseId":"toolu_01",
            "tool":"Bash","title":"Run a shell command",
            "detail":{"command":"rm -rf build"},
            "options":[{"id":"allow","label":"Allow once"},
                       {"id":"allow_always","label":"Always","persistent":true}],
            "default":"deny","risk":"destructive"}"#;
        let Some(Inbound::Request { id, request }) = parse_inbound(line) else {
            panic!("must parse as a request");
        };
        assert_eq!(id, "r4");
        assert_eq!(request.kind, "approval");
        assert_eq!(request.tool.as_deref(), Some("Bash"));
        assert_eq!(request.tool_use_id.as_deref(), Some("toolu_01"));
        assert_eq!(request.detail.unwrap()["command"], "rm -rf build");
        assert_eq!(request.options.len(), 2);
        assert!(request.options[1].persistent);
        assert_eq!(request.risk.as_deref(), Some("destructive"));
    }

    /// The rule that makes version skew survivable: a message type this server
    /// has never heard of is reported, not fatal. Rejecting the line would take
    /// the whole link down over a field the adapter was free to add.
    #[test]
    fn an_unknown_message_type_is_surfaced_rather_than_failing_the_line() {
        let Some(Inbound::Unknown { t }) = parse_inbound(r#"{"t":"telemetry","id":"x"}"#) else {
            panic!("an unknown type must parse as Unknown");
        };
        assert_eq!(t, "telemetry");
    }

    #[test]
    fn unknown_fields_on_a_known_message_are_tolerated() {
        // An adapter adding a field must not need a server release.
        let line = r#"{"t":"request","id":"r5","kind":"approval","title":"x",
                       "somethingNew":{"nested":true},"options":[]}"#;
        assert!(matches!(parse_inbound(line), Some(Inbound::Request { .. })));
    }

    #[test]
    fn a_diagnostic_line_that_is_not_json_is_not_an_error() {
        // The protocol lets adapters write plain diagnostics to stdout.
        assert!(parse_inbound("Warning: model fell back to sonnet").is_none());
        assert!(parse_inbound("").is_none());
    }

    #[test]
    fn a_message_missing_its_type_is_ignored() {
        assert!(parse_inbound(r#"{"id":"x","text":"no type here"}"#).is_none());
    }

    /// An `ok`/`err` with no id cannot be correlated to anything, so it is
    /// dropped rather than matched against an arbitrary pending command.
    #[test]
    fn a_reply_without_an_id_is_dropped_rather_than_mismatched() {
        assert!(parse_inbound(r#"{"t":"ok","adapter":"claude/1"}"#).is_none());
        assert!(parse_inbound(r#"{"t":"err","code":"NOPE"}"#).is_none());
    }

    #[test]
    fn an_error_reply_defaults_its_code_when_the_adapter_omits_one() {
        let Some(Inbound::Err { id, code, message }) =
            parse_inbound(r#"{"t":"err","id":"c3","message":"model unavailable"}"#)
        else {
            panic!("must parse as an error");
        };
        assert_eq!(id, "c3");
        assert_eq!(code, "ADAPTER_ERROR");
        assert_eq!(message, "model unavailable");
    }

    #[test]
    fn capabilities_default_to_empty_so_a_minimal_adapter_is_usable() {
        let caps: SessionCapabilities = serde_json::from_str("{}").unwrap();
        assert!(caps.commands.is_empty());
        assert!(caps.models.is_empty());
        assert!(caps.permission_mode.is_none());
    }

    #[test]
    fn an_unrecognized_capability_string_is_ignored_not_a_parse_failure() {
        // A newer adapter naming an ability we do not model degrades to its
        // known subset instead of failing the handshake.
        let declared: Vec<Capability> =
            serde_json::from_str(r#"["interrupt","time_travel","steer"]"#)
                .map(|v: Vec<serde_json::Value>| {
                    v.into_iter()
                        .filter_map(|item| serde_json::from_value(item).ok())
                        .collect()
                })
                .unwrap();
        assert_eq!(declared, vec![Capability::Interrupt, Capability::Steer]);
    }

    #[test]
    fn a_set_command_puts_the_field_at_the_top_level() {
        let value = line_of(
            &Outbound::Set { field: "model".into(), value: "opus-5".into() },
            "s1",
        );
        assert_eq!(value["t"], "set");
        assert_eq!(value["model"], "opus-5");
    }

    #[test]
    fn interrupt_and_close_need_no_payload_beyond_their_type() {
        assert_eq!(line_of(&Outbound::Interrupt, "i1")["t"], "interrupt");
        assert_eq!(line_of(&Outbound::Close, "z1")["t"], "close");
    }
}
