//! The `sylph.agent.v3` wire format.
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
pub const PROTOCOL: &str = "sylph.agent.v3";

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
    /// Accepts a typed config change on a live session.
    SetConfig,
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

/// Whether a command is something the agent thinks about, or something the
/// session *is*.
///
/// The distinction v2 lacked, and the reason `/model` answered with a paragraph:
/// sent as prompt text to a headless CLI, a request to pick a model can only come
/// back as prose, because there is no picker to open. A `Control` command instead
/// names a [`ConfigOption`] the client renders directly.
///
/// Defaults to [`CommandKind::Prompt`], so an adapter that says nothing behaves
/// exactly as it did before this field existed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    /// Delivered as a turn. Its output is durable history.
    #[default]
    Prompt,
    /// Never a turn. Resolves against typed session config.
    Control,
}

/// A slash command the agent itself reports.
///
/// On a real project the handshake returns dozens of these — skills, project
/// commands, plugins — where v1's `--describe` returned the one or two an adapter
/// author hand-wrote.
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
    /// Prompt or control. Absent means prompt.
    #[serde(default)]
    pub kind: CommandKind,
    /// For a control command, the [`ConfigOption`] it drives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_id: Option<String>,
    /// Other names the agent answers to for this command.
    ///
    /// Carried because agents ship them and a terminal honours them — `claude`
    /// gives `/usage` the aliases `cost` and `stats`. A menu that ignores them
    /// behaves unlike the terminal it is modelled on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// One choice within a [`ConfigOption::Select`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigChoice {
    pub value: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Something adjustable on a live session.
///
/// The shape is ACP's `SessionConfigOption`, adopted rather than invented: it
/// already covers `claude`'s model / effort / permission mode / output style,
/// `pi`'s model / thinking level / queue modes, `codex`'s approval policy, and
/// `gemini`'s approval mode. Two variants are enough for every knob those four
/// actually expose.
///
/// `category` is advisory grouping — `model`, `effort`, `permission_mode`, or
/// anything an agent invents — so a client groups by it without having to know the
/// vocabulary. That openness is the point: the next agent's knobs render without a
/// server or UI change.
/// `rename_all_fields` is load-bearing, not tidiness: with only the container
/// `rename_all`, the *variant names* are renamed but the fields inside them are
/// not, so `currentValue` on the wire parses as `None` while everything still
/// compiles and every other field works. A test caught it; review would not have.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ConfigOption {
    Select {
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        category: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_value: Option<String>,
        #[serde(default)]
        options: Vec<ConfigChoice>,
    },
    Boolean {
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        category: Option<String>,
        #[serde(default)]
        current_value: bool,
    },
}

impl ConfigOption {
    pub fn id(&self) -> &str {
        match self {
            Self::Select { id, .. } | Self::Boolean { id, .. } => id,
        }
    }
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
/// Pushed rather than polled: a skill edited on disk or a plugin loading changes
/// what the composer should offer, and a list the client refreshes on its own
/// schedule is wrong for however long that schedule is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub subagents: Vec<SubagentSpec>,
    /// Everything adjustable on this session, typed.
    #[serde(default)]
    pub config: Vec<ConfigOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
}

/// A capability update, addressed by facet.
///
/// Every field is an `Option` and **`None` means "not mentioned"**, which is the
/// whole design. v2 carried the entire surface in one value and assigned it, so an
/// adapter reporting a permission-mode change also declared that the session had no
/// commands — and the symptom surfaced two layers away as a slash menu that would
/// not open, long after the message that caused it.
///
/// Merging partial surfaces would have fixed that same bug at the same cost and
/// left the hazard in place: a message about one part of the surface would still be
/// *able* to speak for all of it, and the next adapter would rediscover it. Naming
/// the facet is what makes silence mean silence.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityFacets {
    #[serde(default)]
    pub commands: Option<Vec<CommandSpec>>,
    #[serde(default)]
    pub subagents: Option<Vec<SubagentSpec>>,
    #[serde(default)]
    pub config: Option<Vec<ConfigOption>>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub context_window: Option<i64>,
}

impl CapabilityFacets {
    /// Whether this update mentions anything at all.
    ///
    /// An update naming no facet is a no-op rather than a clear, so a malformed or
    /// empty payload cannot erase a surface by accident.
    pub fn is_empty(&self) -> bool {
        self.commands.is_none()
            && self.subagents.is_none()
            && self.config.is_none()
            && self.permission_mode.is_none()
            && self.context_window.is_none()
    }

    /// Apply the mentioned facets onto `surface`, leaving the rest alone.
    pub fn apply_to(self, surface: &mut SessionCapabilities) {
        if let Some(commands) = self.commands {
            surface.commands = commands;
        }
        if let Some(subagents) = self.subagents {
            surface.subagents = subagents;
        }
        if let Some(config) = self.config {
            surface.config = config;
        }
        if let Some(mode) = self.permission_mode {
            surface.permission_mode = Some(mode);
        }
        if let Some(window) = self.context_window {
            surface.context_window = Some(window);
        }
    }
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
    /// What this option means, when the agent explained it.
    ///
    /// Load-bearing on a `choice`, where the agent authored the options and the
    /// description is most of what makes them readable — "Tabs" and "Spaces" say far
    /// less than the sentences beside them. Omitting the field here did not merely
    /// fail to use it: the options are re-serialized from this struct on their way to
    /// the journal, so a description the adapter sent was silently discarded before
    /// any client could render it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    /// Whether a lasting grant may be offered at all.
    ///
    /// `false` means a client must omit the "always allow" affordance entirely,
    /// not merely de-emphasize it: `claude` sets it on asks where accepting always
    /// would write a permission rule *broader than the ask itself*, so offering it
    /// turns a narrow yes into a wide one the user never read. Defaults to `true`,
    /// which keeps a minimal adapter correct.
    #[serde(default = "default_true")]
    pub allow_persistent: bool,
    /// When the agent stops waiting, if it will.
    ///
    /// Advisory and never acted on by the server. `pi` auto-resolves a dialog when
    /// its own timeout lapses, so a client with no deadline to show is racing a
    /// clock it cannot see. The server does not apply a default when it passes —
    /// that would remove the user from the decision this channel exists to give
    /// them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

fn default_true() -> bool {
    true
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
        /// Config overrides for this turn, sent with it rather than as separate
        /// preceding calls: those were extra round trips with a window in between
        /// where a turn could run under a mix of old and new settings.
        config: HashMap<String, serde_json::Value>,
    },
    Interrupt,
    Steer {
        text: String,
        /// The turn this steer is meant for.
        ///
        /// Without it, a steer racing a turn boundary is delivered into the *next*
        /// turn: the message arrives, nothing errors, and it lands in the wrong
        /// conversation. codex requires the same precondition.
        expected_turn_id: Option<String>,
    },
    SetConfig {
        config_id: String,
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
                config,
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
                if !config.is_empty() {
                    object.insert("config".into(), serde_json::to_value(config).unwrap_or_default());
                }
                "send"
            }
            Self::Interrupt => "interrupt",
            Self::Steer {
                text,
                expected_turn_id,
            } => {
                object.insert("text".into(), text.clone().into());
                if let Some(turn_id) = expected_turn_id {
                    object.insert("expectedTurnId".into(), turn_id.clone().into());
                }
                "steer"
            }
            Self::SetConfig { config_id, value } => {
                object.insert("configId".into(), config_id.clone().into());
                object.insert("value".into(), value.clone());
                "set_config"
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
            config: HashMap::new(),
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
            config: HashMap::new(),
        };
        let value = line_of(&send, "c1");
        assert_eq!(value["text"], "hello");
        assert!(value.get("command").is_none());
        assert!(value.get("attachments").is_none());
        assert!(value.get("config").is_none());
    }

    /// Config rides along with the turn rather than arriving as separate calls
    /// before it. The previous shape left a window where a turn could begin under a
    /// mix of old and new settings.
    #[test]
    fn per_turn_config_travels_with_the_turn_that_needs_it() {
        let send = Outbound::Send {
            text: "go".into(),
            attachments: vec![],
            command: None,
            args: HashMap::new(),
            config: HashMap::from([("model".to_string(), serde_json::json!("sonnet"))]),
        };
        let value = line_of(&send, "c2");
        assert_eq!(value["config"]["model"], "sonnet");
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
        assert!(caps.config.is_empty());
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
    fn a_set_config_command_names_the_option_and_its_value() {
        let value = line_of(
            &Outbound::SetConfig { config_id: "model".into(), value: "opus-5".into() },
            "s1",
        );
        assert_eq!(value["t"], "set_config");
        assert_eq!(value["configId"], "model");
        assert_eq!(value["value"], "opus-5");
    }

    fn prompt_command(name: &str) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            description: String::new(),
            arg_hint: None,
            source: None,
            kind: CommandKind::Prompt,
            config_id: None,
            aliases: vec![],
        }
    }

    /// The regression this protocol version exists for.
    ///
    /// A partial update used to be applied by assigning the whole surface, so an
    /// adapter reporting only a permission-mode change also declared that the
    /// session had no slash commands. On a real project that erased 46 of them, and
    /// the symptom surfaced two layers away as a composer menu that would not open.
    #[test]
    fn a_facet_update_leaves_the_facets_it_does_not_mention_alone() {
        let mut surface = SessionCapabilities {
            commands: vec![prompt_command("verify")],
            subagents: vec![SubagentSpec { name: "Explore".into(), description: String::new() }],
            config: vec![],
            permission_mode: Some("default".into()),
            context_window: Some(1_000_000),
        };

        let facets: CapabilityFacets =
            serde_json::from_str(r#"{"permissionMode":"plan"}"#).unwrap();
        facets.apply_to(&mut surface);

        assert_eq!(surface.permission_mode.as_deref(), Some("plan"));
        assert_eq!(surface.commands.len(), 1, "commands must survive a mode-only update");
        assert_eq!(surface.subagents.len(), 1);
        assert_eq!(surface.context_window, Some(1_000_000));
    }

    /// An explicitly empty list is a real value: an agent whose plugins were
    /// unloaded genuinely has no commands, and that must stay expressible.
    #[test]
    fn an_explicitly_empty_facet_does_clear_that_facet() {
        let mut surface =
            SessionCapabilities { commands: vec![prompt_command("verify")], ..Default::default() };
        let facets: CapabilityFacets = serde_json::from_str(r#"{"commands":[]}"#).unwrap();
        facets.apply_to(&mut surface);
        assert!(surface.commands.is_empty());
    }

    /// Distinguishing "said nothing" from "said empty" is the whole mechanism, so it
    /// is asserted directly rather than only through `apply_to`.
    #[test]
    fn a_facet_update_naming_nothing_is_a_no_op() {
        let facets: CapabilityFacets = serde_json::from_str("{}").unwrap();
        assert!(facets.is_empty());
        let named: CapabilityFacets = serde_json::from_str(r#"{"commands":[]}"#).unwrap();
        assert!(!named.is_empty(), "an empty list is still a statement about commands");
    }

    /// Absent `kind` means prompt, so an adapter that never heard of the field
    /// behaves exactly as it did before the field existed.
    #[test]
    fn a_command_with_no_kind_is_a_prompt_command() {
        let command: CommandSpec =
            serde_json::from_str(r#"{"name":"verify","description":"check it"}"#).unwrap();
        assert_eq!(command.kind, CommandKind::Prompt);
        assert!(command.config_id.is_none());
    }

    #[test]
    fn a_control_command_names_the_config_option_it_drives() {
        let command: CommandSpec = serde_json::from_str(
            r#"{"name":"model","kind":"control","configId":"model","aliases":["m"]}"#,
        )
        .unwrap();
        assert_eq!(command.kind, CommandKind::Control);
        assert_eq!(command.config_id.as_deref(), Some("model"));
        assert_eq!(command.aliases, vec!["m"]);
    }

    #[test]
    fn a_select_config_option_carries_its_choices_and_current_value() {
        let option: ConfigOption = serde_json::from_str(
            r#"{"type":"select","id":"model","name":"Model","category":"model",
                "currentValue":"sonnet",
                "options":[{"value":"sonnet","name":"Sonnet 5"}]}"#,
        )
        .unwrap();
        assert_eq!(option.id(), "model");
        let ConfigOption::Select { current_value, options, .. } = &option else {
            panic!("must parse as a select");
        };
        assert_eq!(current_value.as_deref(), Some("sonnet"));
        assert_eq!(options[0].name, "Sonnet 5");
    }

    /// Absent means allowed, so a minimal adapter that never sets it stays correct;
    /// an explicit `false` is what forbids the affordance.
    /// A `choice`'s options carry the agent's own explanations, and they must survive
    /// the trip through this struct: the options are re-serialized from here into the
    /// journal, so a field missing from the type is a field the client never sees.
    /// That is how the descriptions on an AskUserQuestion were being dropped —
    /// silently, with the adapter sending them correctly.
    #[test]
    fn an_option_keeps_the_explanation_the_agent_wrote_for_it() {
        let line = r#"{"t":"request","id":"r7","kind":"choice","title":"Indentation?",
            "options":[{"id":"option:0","label":"Tabs","description":"Use tab characters"},
                       {"id":"option:1","label":"Spaces"}]}"#;
        let Some(Inbound::Request { request, .. }) = parse_inbound(line) else {
            panic!("must parse as a request");
        };
        assert_eq!(request.options[0].description.as_deref(), Some("Use tab characters"));
        assert!(request.options[1].description.is_none());

        // The round trip is the actual promise: this is what reaches the journal.
        let json = serde_json::to_value(&request.options).unwrap();
        assert_eq!(json[0]["description"], "Use tab characters");
        assert!(json[1].get("description").is_none(), "an absent one stays absent");
    }

    #[test]
    fn an_approval_may_forbid_offering_a_lasting_grant() {
        let permissive: AdapterRequest =
            serde_json::from_str(r#"{"kind":"approval","title":"x"}"#).unwrap();
        assert!(permissive.allow_persistent);

        let narrow: AdapterRequest =
            serde_json::from_str(r#"{"kind":"approval","title":"x","allowPersistent":false}"#)
                .unwrap();
        assert!(!narrow.allow_persistent);
    }

    /// A steer names the turn it was written for, so one racing a turn boundary is
    /// refused rather than delivered into the next conversation.
    #[test]
    fn a_steer_carries_the_turn_the_client_believed_was_running() {
        let value = line_of(
            &Outbound::Steer { text: "actually, stop".into(), expected_turn_id: Some("t7".into()) },
            "c4",
        );
        assert_eq!(value["t"], "steer");
        assert_eq!(value["expectedTurnId"], "t7");

        let unscoped =
            line_of(&Outbound::Steer { text: "hi".into(), expected_turn_id: None }, "c5");
        assert!(unscoped.get("expectedTurnId").is_none());
    }

    #[test]
    fn interrupt_and_close_need_no_payload_beyond_their_type() {
        assert_eq!(line_of(&Outbound::Interrupt, "i1")["t"], "interrupt");
        assert_eq!(line_of(&Outbound::Close, "z1")["t"], "close");
    }
}
