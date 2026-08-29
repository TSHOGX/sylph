//! `claude`'s `initialize` reply, projected onto a [`SessionCapabilities`].
//!
//! The capability surface: `commands_from`, `models_from`,
//! `config_from` and `capabilities_from` (roughly :1546-1716). Pure functions over one
//! payload, for the same reason [`super::translate`] is: **reading the wrong key off a
//! vendor payload fails silently**, so the projection has to be assertable against a
//! payload that was printed rather than against a live CLI that may not produce the
//! shape under test.
//!
//! # Every key here was printed from claude 2.1.251, not inferred
//!
//! One `initialize` round trip against the real CLI, launched exactly as
//! [`super::ClaudeAdapter::argv_for`] launches it. The reply's top-level keys were:
//! `account`, `agents`, `analytics_disabled`, `available_output_styles`, `commands`,
//! `current_permission_mode`, `fast_mode_disabled_reason`, `fast_mode_state`,
//! `ide_rc_auto_enable_gate`, `models`, `output_style`, `pid`,
//! `remote_control_auto_enable`, `remote_control_auto_on_by_default`,
//! `remote_control_available`, `session_state`.
//!
//! **`current_model` is not among them, and neither is any effort field.** That is the
//! measurement this module is shaped around and it is stated at [`config_from`], because
//! it decides where a current value may come from at all.

use serde_json::Value;
use sylph_protocol::{
    CommandKind, CommandSpec, ConfigChoice, ConfigKind, ConfigOption, SessionCapabilities,
    SubagentSpec,
};

use super::{BUILTIN_SOURCES, CONTROL_COMMANDS, SETTABLE_PERMISSION_MODES};

/// Read a string field, treating an empty string as absent.
///
/// The same rule [`super::translate`] uses, and for the same reason: this CLI sends `""`
/// and omits a field interchangeably — measured, every one of 47 reported commands
/// carried `argumentHint` and 44 of them carried it as `""`. An empty hint rendered as a
/// hint is a control that claims to take an argument it does not.
fn text_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// One model the account may select.
///
/// **The entries are `{value, displayName, …}`, never `{id, label}`.** Reading `id`
/// produced an empty model list on every session ever opened, presenting two layers away
/// as a broken composer control — the most-cited instance of the vendor-key family in
/// `AGENTS.md`. Printed from 2.1.251, an entry is `{"value","resolvedModel",
/// "displayName","description","supportsEffort","supportedEffortLevels",
/// "supportsAdaptiveThinking","supportsFastMode","supportsAutoMode"}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
    /// The effort levels **this model** supports.
    ///
    /// Per model rather than per session: `docs/session-protocol.md` § Session config
    /// states the rule, and this CLI is why it exists — `supportedEffortLevels` is a
    /// field on a model entry, so a session-global effort enumeration is wrong for any
    /// account whose models differ.
    pub effort_levels: Vec<String>,
}

/// The model catalogue, in the CLI's own spelling.
pub fn models_from(initialize: &Value) -> Vec<Model> {
    let Some(entries) = initialize.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            // A bare string is accepted because an older CLI listed models that way and
            // the cost of tolerating it is one branch. It is not a dual *spelling* of a
            // field, which `AGENTS.md` § Compatibility forbids — it is a different JSON
            // type for the whole entry.
            if let Some(name) = entry.as_str() {
                return Some(Model {
                    value: name.to_string(),
                    name: name.to_string(),
                    description: None,
                    effort_levels: Vec::new(),
                });
            }
            let value = text_field(entry, "value")?;
            Some(Model {
                name: text_field(entry, "displayName").unwrap_or_else(|| value.clone()),
                description: text_field(entry, "description"),
                effort_levels: entry
                    .get("supportedEffortLevels")
                    .and_then(Value::as_array)
                    .map(|levels| {
                        levels
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                value,
            })
        })
        .collect()
}

/// The slash commands this session has, classified.
///
/// The classification is a **vendor fact and the adapter's job**: `claude` returns
/// `/model` in the same list as a user's skill with no field telling them apart —
/// measured, an entry carries exactly `name`, `description`, `argumentHint`, `aliases`.
/// So `kind` cannot be discovered and [`CONTROL_COMMANDS`] is the table that decides it.
/// Getting it wrong sends `/model` through as prose, which answers with a paragraph
/// listing models — the CLI behaving correctly, because a headless client has no picker
/// to open.
pub fn commands_from(initialize: &Value) -> Vec<CommandSpec> {
    let Some(entries) = initialize.get("commands").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            if let Some(name) = entry.as_str() {
                return Some(CommandSpec {
                    name: name.to_string(),
                    description: String::new(),
                    arg_hint: None,
                    source: None,
                    kind: CommandKind::Prompt,
                    config_id: None,
                    aliases: Vec::new(),
                });
            }
            let name = text_field(entry, "name")?;
            let control = CONTROL_COMMANDS
                .iter()
                .find(|(command, _)| *command == name.as_str());
            Some(CommandSpec {
                description: entry
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                // `argumentHint` is this CLI's spelling, printed rather than guessed.
                arg_hint: text_field(entry, "argumentHint"),
                // Advisory only, and known-incomplete — see [`BUILTIN_SOURCES`], where
                // the measured coverage and the reason it is not "fixed" by hand-listing
                // the rest are recorded.
                source: Some(
                    if BUILTIN_SOURCES.contains(&name.as_str()) {
                        "builtin"
                    } else {
                        "skill"
                    }
                    .to_string(),
                ),
                kind: match control {
                    Some(_) => CommandKind::Control,
                    None => CommandKind::Prompt,
                },
                config_id: control.map(|(_, option)| (*option).to_string()),
                // Carried because the terminal honours them: `/usage` answers to `cost`
                // and `stats`, so a menu that ignores them behaves unlike the terminal it
                // is modelled on. Measured on 2.1.251: `code-review` reports `review`.
                aliases: entry
                    .get("aliases")
                    .and_then(Value::as_array)
                    .map(|names| {
                        names
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                name,
            })
        })
        .collect()
}

/// The subagents this session may delegate to.
///
/// **`initialize` and `system/init` disagree about the key, and only one of them is this
/// payload.** Printed from 2.1.251: the `initialize` reply carries
/// `{"name","description","model"}`, while the `system/init` *event* carries
/// `{"agentType"}` — the binary builds the latter as `agents:ht.map(ct=>({agentType:
/// ct.name}))`. A reader that took `agentType` here would find nothing on every session,
/// which is the vendor-key failure by yet another route. Nothing reads `system/init`'s
/// agents list, and this note is why it must not start doing so with this function.
pub fn subagents_from(initialize: &Value) -> Vec<SubagentSpec> {
    let Some(entries) = initialize.get("agents").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            if let Some(name) = entry.as_str() {
                return Some(SubagentSpec {
                    name: name.to_string(),
                    description: String::new(),
                });
            }
            Some(SubagentSpec {
                name: text_field(entry, "name")?,
                description: entry
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect()
}

/// Everything adjustable on this live session, typed.
///
/// `observed_model` is what the **vendor** last said it is running, which arrives on
/// `system/init` as [`super::Emission::observed_model`]. It is a parameter rather than
/// state here because a pure function is what makes the read-back rule checkable.
///
/// # Where a current value may come from, and where it may not
///
/// *Measured on claude 2.1.251, launched with `--model sonnet --effort high`:* the
/// `initialize` reply reported **neither**. Re-issuing `initialize` after a successful
/// `set_model` still reported no `current_model`. So for two of the four options the
/// vendor offers no read-back at all through this channel, and the rule
/// (`docs/session-protocol.md` § Three rules about reading a vendor's answer) leaves
/// exactly one honest answer:
///
/// - **`permissionMode` reads `current_permission_mode`**, which is genuinely confirmed
///   — set `bypassPermissions`, re-initialize, and the reply says `bypassPermissions`.
///   This is the one option whose current value is a vendor fact here.
/// - **`model` reads `observed_model`** and is absent until a turn has produced a
///   `system/init`. Writing what was last *set* is the defect `AGENTS.md` records twice:
///   it makes the adapter the authority on a fact only the vendor holds.
/// - **`effort` is always absent.** No vendor surface reports it — not `initialize`, not
///   `system/init`, not `apply_flag_settings`' reply, which carries no payload at all.
///   Reporting the requested value would put a checkmark on a row nothing confirmed;
///   absence says "the vendor did not say", which is true. The compensation is on the
///   write side: [`super::session`] validates an effort against the selected model's
///   `supportedEffortLevels` before sending, because `apply_flag_settings` answers
///   **success** for `{"effort":"banana"}` — measured — so an unvalidated write is a
///   refusal indistinguishable from an applied set.
/// - **`fastMode` reads `fast_mode_state`**, which is reported.
pub fn config_from(initialize: &Value, observed_model: Option<&str>) -> Vec<ConfigOption> {
    let models = models_from(initialize);
    let mut options = Vec::new();

    // Which model's effort levels to *offer*. Deliberately a different question from
    // what the current value is: the levels have to come from some model or the option
    // cannot be rendered at all, while a current value that was not confirmed must stay
    // absent. Conflating the two is how a fallback becomes a claim.
    let offering = observed_model
        .map(str::to_string)
        .or_else(|| {
            models
                .iter()
                .find(|model| model.value == "default")
                .map(|model| model.value.clone())
        })
        .or_else(|| models.first().map(|model| model.value.clone()));

    if !models.is_empty() {
        options.push(ConfigOption {
            id: "model".to_string(),
            name: "Model".to_string(),
            description: None,
            category: Some("model".to_string()),
            kind: ConfigKind::Select,
            current_value: observed_model.map(|model| Value::String(model.to_string())),
            options: models
                .iter()
                .map(|model| ConfigChoice {
                    value: model.value.clone(),
                    name: model.name.clone(),
                    description: model.description.clone(),
                })
                .collect(),
        });
    }

    // Omitted rather than shown empty when the model supports no effort: an empty picker
    // invites a choice that cannot exist.
    let levels = offering
        .as_deref()
        .and_then(|selected| models.iter().find(|model| model.value == selected))
        .map(|model| model.effort_levels.clone())
        .unwrap_or_default();
    if !levels.is_empty() {
        options.push(ConfigOption {
            id: "effort".to_string(),
            name: "Reasoning effort".to_string(),
            description: None,
            category: Some("effort".to_string()),
            kind: ConfigKind::Select,
            // Absent, always. See this function's doc comment: no vendor surface reports
            // it, and a requested value written here would be a checkmark on a row
            // nothing confirmed.
            current_value: None,
            options: levels
                .iter()
                .map(|level| ConfigChoice {
                    value: level.clone(),
                    name: capitalize(level),
                    description: None,
                })
                .collect(),
        });
    }

    // Offered from the **settable** set, never from what `--permission-mode` parses at
    // launch. Two different questions: the launch list accepts `manual`, which the vendor
    // silently normalizes to `default` — measured, it answers success and confirms
    // `default` — and omits `auto`, which it applies as given.
    options.push(ConfigOption {
        id: "permissionMode".to_string(),
        name: "Permissions".to_string(),
        description: None,
        category: Some("permission_mode".to_string()),
        kind: ConfigKind::Select,
        current_value: text_field(initialize, "current_permission_mode").map(Value::String),
        options: SETTABLE_PERMISSION_MODES
            .iter()
            .map(|mode| ConfigChoice {
                value: (*mode).to_string(),
                name: (*mode).to_string(),
                description: None,
            })
            .collect(),
    });

    // Output style is deliberately **not** offered, although `available_output_styles` is
    // reported (measured: `["default","Proactive","Concise","Explanatory","Learning"]`).
    // Probing every plausible control subtype found none that sets it, and an option a
    // client can render but not apply is worse than an absent one — the picker would
    // accept a choice that silently does nothing. It becomes an option the day a setter
    // is confirmed.

    if let Some(state) = text_field(initialize, "fast_mode_state") {
        options.push(ConfigOption {
            id: "fastMode".to_string(),
            name: "Fast mode".to_string(),
            // The vendor's third state, said plainly rather than encoded in the boolean.
            // `fast_mode_state` is `on` | `off` | `cooldown` — the binary's own validation
            // message names all three — and a boolean cannot hold the last one. `cooldown`
            // reads as off because that is what a user can act on, and the reason is
            // surfaced here so the difference is legible instead of lost.
            description: match state.as_str() {
                "cooldown" => Some(
                    text_field(initialize, "fast_mode_disabled_reason")
                        .unwrap_or_else(|| "In cooldown.".to_string()),
                ),
                _ => None,
            },
            category: Some("speed".to_string()),
            kind: ConfigKind::Boolean,
            current_value: Some(Value::Bool(state == "on")),
            options: Vec::new(),
        });
    }

    options
}

/// The whole surface an open session reports.
pub fn capabilities_from(initialize: &Value, observed_model: Option<&str>) -> SessionCapabilities {
    SessionCapabilities {
        commands: commands_from(initialize),
        subagents: subagents_from(initialize),
        config: config_from(initialize, observed_model),
        permission_mode: text_field(initialize, "current_permission_mode"),
        // Not reported by this CLI's `initialize` — printed and checked, so absent rather
        // than a plausible constant. A synthesized window would be a lie a journal makes
        // permanent, and the UI reads absence as "unknown" already.
        context_window: None,
    }
}

/// Title-case one effort level for display, leaving its value untouched.
fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One real `initialize` reply, trimmed to the fields this module reads.
    ///
    /// **Printed from claude 2.1.251, not composed from the source.** The whole point of
    /// this module's tests is that they assert against a payload the vendor produced, since
    /// a reader for a key the vendor does not send fails silently rather than raising.
    fn printed_initialize() -> Value {
        json!({
            "commands": [
                {"name": "model", "description": "Set the model", "argumentHint": ""},
                {"name": "code-review", "description": "Review the diff",
                 "argumentHint": "[low|medium|high]", "aliases": ["review"]},
                {"name": "obsidian", "description": "Maintain the vault (user)",
                 "argumentHint": ""}
            ],
            "agents": [
                {"name": "claude", "description": "Catch-all for any task"},
                {"name": "Explore", "description": "Fast read-only search agent",
                 "model": "inherit"}
            ],
            "models": [
                {"value": "default", "resolvedModel": "claude-opus-5[1m]",
                 "displayName": "Default (recommended)",
                 "description": "Use the default model",
                 "supportsEffort": true,
                 "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]},
                {"value": "sonnet", "resolvedModel": "claude-sonnet-5",
                 "displayName": "Sonnet", "description": "Efficient for routine tasks",
                 "supportsEffort": true,
                 "supportedEffortLevels": ["low", "medium", "high"]},
                // Trimmed of `displayName` and `supportedEffortLevels` on purpose, so the
                // absent cases are exercised rather than only the fully populated ones.
                {"value": "haiku"}
            ],
            "output_style": "default",
            "available_output_styles": ["default", "Proactive", "Concise"],
            "current_permission_mode": "default",
            "fast_mode_state": "off",
            "session_state": "idle",
            "pid": 1234
        })
    }

    fn option<'a>(options: &'a [ConfigOption], id: &str) -> &'a ConfigOption {
        options
            .iter()
            .find(|option| option.id == id)
            .unwrap_or_else(|| panic!("no {id} option in {:?}", ids(options)))
    }

    fn ids(options: &[ConfigOption]) -> Vec<&str> {
        options.iter().map(|option| option.id.as_str()).collect()
    }

    /// The most-cited vendor-key defect in `AGENTS.md`, pinned on the key itself.
    ///
    /// `capabilities_from` read `entry["id"]` from this catalogue, which answers
    /// `{"value","displayName",…}`. The result was an empty model list on **every session
    /// ever opened**, presenting as a UI bug in the composer's model control. Asserted on
    /// the values a printed payload should have produced, never on the count alone: a
    /// reader for the wrong key yields an empty list, which a length assertion catches, but
    /// a reader for the wrong *display* key yields plausible rows.
    #[test]
    fn the_model_catalogue_is_read_from_value_and_display_name() {
        let models = models_from(&printed_initialize());
        assert_eq!(
            models.iter().map(|m| m.value.as_str()).collect::<Vec<_>>(),
            vec!["default", "sonnet", "haiku"]
        );
        assert_eq!(models[0].name, "Default (recommended)");
        assert_eq!(models[1].name, "Sonnet");
        // No `displayName`, so the value stands in rather than an empty label.
        assert_eq!(models[2].name, "haiku");
    }

    /// The failing shape, stated as a test so the defect cannot return silently.
    #[test]
    fn a_catalogue_spelled_with_id_and_label_yields_nothing() {
        let wrong = json!({"models": [{"id": "sonnet", "label": "Sonnet"}]});
        assert!(
            models_from(&wrong).is_empty(),
            "reading `id` must not appear to work"
        );
    }

    /// Effort levels are **per model**, so the option is scoped to the selected one.
    #[test]
    fn effort_levels_come_from_the_selected_model_not_the_session() {
        let payload = printed_initialize();
        // No observed model: the `default` entry decides which levels are offered.
        let offered = config_from(&payload, None);
        let values: Vec<&str> = option(&offered, "effort")
            .options
            .iter()
            .map(|choice| choice.value.as_str())
            .collect();
        assert_eq!(values, vec!["low", "medium", "high", "xhigh", "max"]);

        // Sonnet reports three, so the option shrinks with the selection.
        let narrowed = config_from(&payload, Some("sonnet"));
        assert_eq!(option(&narrowed, "effort").options.len(), 3);

        // A model reporting none omits the option entirely rather than showing an empty
        // picker, which would invite a choice that cannot exist.
        let none = config_from(&payload, Some("haiku"));
        assert!(
            !none.iter().any(|option| option.id == "effort"),
            "got {:?}",
            ids(&none)
        );
    }

    /// **The model's current value is the vendor's, and absent until it says so.**
    ///
    /// *Measured on 2.1.251:* `initialize` reports no `current_model`, not on open and not
    /// after a successful `set_model`. `system/init` is the only source, so before the first
    /// turn the honest answer is absence — writing the requested value would put a
    /// checkmark on a row nothing confirmed.
    #[test]
    fn the_model_current_value_is_absent_until_the_vendor_reports_one() {
        let payload = printed_initialize();
        assert_eq!(option(&config_from(&payload, None), "model").current_value, None);
        assert_eq!(
            option(&config_from(&payload, Some("sonnet")), "model").current_value,
            Some(json!("sonnet")),
            "an observed model is the one value that may be reported"
        );
    }

    /// Effort has **no** vendor read-back, so it is absent unconditionally.
    ///
    /// Not a placeholder for a value that could be filled in later: no surface on this CLI
    /// reports it — `initialize` does not, `system/init` does not, and
    /// `apply_flag_settings` answers with no payload at all. The write side compensates by
    /// validating against the enumeration.
    #[test]
    fn effort_reports_no_current_value_because_the_vendor_reports_none() {
        assert_eq!(
            option(&config_from(&printed_initialize(), None), "effort").current_value,
            None
        );
    }

    /// The permission mode is the one option whose current value **is** confirmed.
    #[test]
    fn the_permission_mode_is_read_back_from_the_vendor() {
        let mut payload = printed_initialize();
        payload["current_permission_mode"] = json!("bypassPermissions");
        assert_eq!(
            option(&config_from(&payload, None), "permissionMode").current_value,
            Some(json!("bypassPermissions"))
        );
    }

    /// The offered modes are the **settable** ones, not what `--permission-mode` parses.
    ///
    /// Two measured facts make the lists differ: `manual` parses at launch and is silently
    /// normalized to `default`, so offering it lands a picker on a different row; and `auto`
    /// is settable and was missing, so a real mode was unreachable.
    #[test]
    fn the_permission_picker_offers_only_modes_the_vendor_applies_as_given() {
        let offered = config_from(&printed_initialize(), None);
        let values: Vec<&str> = option(&offered, "permissionMode")
            .options
            .iter()
            .map(|choice| choice.value.as_str())
            .collect();
        assert!(!values.contains(&"manual"), "got {values:?}");
        assert!(values.contains(&"auto"));
        assert!(values.contains(&"bypassPermissions"));
    }

    /// `/model` must be `control`, or it is sent as prose and answers with a paragraph
    /// listing models — the CLI behaving correctly, because a headless client has no picker.
    #[test]
    fn a_control_command_is_classified_and_names_its_option() {
        let commands = commands_from(&printed_initialize());
        let model = commands.iter().find(|c| c.name == "model").expect("/model");
        assert_eq!(model.kind, CommandKind::Control);
        assert_eq!(model.config_id.as_deref(), Some("model"));

        let review = commands
            .iter()
            .find(|c| c.name == "code-review")
            .expect("/code-review");
        assert_eq!(review.kind, CommandKind::Prompt);
        assert!(review.config_id.is_none());
        // Carried because the terminal honours them.
        assert_eq!(review.aliases, vec!["review"]);
    }

    /// An empty `argumentHint` is **absent**, not an empty hint.
    ///
    /// Measured: 44 of 47 reported commands carry it as `""`. A control that renders an
    /// empty hint claims to take an argument it does not.
    #[test]
    fn an_empty_argument_hint_is_absent_rather_than_empty() {
        let commands = commands_from(&printed_initialize());
        let model = commands.iter().find(|c| c.name == "model").unwrap();
        assert_eq!(model.arg_hint, None);
        let review = commands.iter().find(|c| c.name == "code-review").unwrap();
        assert_eq!(review.arg_hint.as_deref(), Some("[low|medium|high]"));
    }

    /// The `initialize` reply spells a subagent `{name, description}`.
    ///
    /// The `system/init` **event** spells the same thing `{agentType}`, so a reader for that
    /// key would find nothing on every session — the vendor-key failure by another route.
    #[test]
    fn subagents_are_read_from_the_key_the_initialize_reply_uses() {
        let subagents = subagents_from(&printed_initialize());
        assert_eq!(
            subagents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["claude", "Explore"]
        );
        assert!(subagents[1].description.starts_with("Fast read-only"));

        let event_shape = json!({"agents": [{"agentType": "Explore"}]});
        assert!(
            subagents_from(&event_shape).is_empty(),
            "the event's spelling must not appear to work here"
        );
    }

    /// Fast mode's third state, said rather than silently folded into the boolean.
    #[test]
    fn a_fast_mode_cooldown_reads_as_off_and_explains_itself() {
        let mut payload = printed_initialize();
        payload["fast_mode_state"] = json!("cooldown");
        payload["fast_mode_disabled_reason"] = json!("Budget exhausted until 14:00.");
        let cooling = config_from(&payload, None);
        let fast = option(&cooling, "fastMode");
        assert_eq!(fast.current_value, Some(json!(false)));
        assert_eq!(fast.description.as_deref(), Some("Budget exhausted until 14:00."));

        payload["fast_mode_state"] = json!("on");
        assert_eq!(
            option(&config_from(&payload, None), "fastMode").current_value,
            Some(json!(true))
        );
    }

    /// An unreported field means the option is absent, never defaulted to `false`.
    ///
    /// `false` is a claim about the session and is indistinguishable from the vendor
    /// genuinely reporting off.
    #[test]
    fn an_unreported_fast_mode_is_not_offered_at_all() {
        let mut payload = printed_initialize();
        payload.as_object_mut().unwrap().remove("fast_mode_state");
        let options = config_from(&payload, None);
        assert!(
            !options.iter().any(|option| option.id == "fastMode"),
            "got {:?}",
            ids(&options)
        );
    }

    /// The context window is not reported by this CLI, so it is absent rather than a
    /// plausible constant — a synthesized value is a lie a journal makes permanent.
    #[test]
    fn a_surface_omits_what_the_vendor_did_not_report() {
        let surface = capabilities_from(&printed_initialize(), None);
        assert_eq!(surface.context_window, None);
        assert_eq!(surface.permission_mode.as_deref(), Some("default"));
        assert_eq!(surface.commands.len(), 3);
        assert_eq!(surface.subagents.len(), 2);
    }

    /// A reply missing every list is a usable minimal surface, not a panic.
    #[test]
    fn an_empty_reply_still_yields_the_permission_option() {
        let options = config_from(&json!({}), None);
        assert_eq!(ids(&options), vec!["permissionMode"]);
        assert_eq!(option(&options, "permissionMode").current_value, None);
    }
}
