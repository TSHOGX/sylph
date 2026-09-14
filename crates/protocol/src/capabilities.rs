//! What a session can do, and what is adjustable on it while it runs.
//!
//! Once the `sylph.agent.v3` payload vocabulary of a per-session adapter subprocess.
//! That transport went away — an adapter is a linked-in module now
//! (`docs/agents.md` § What every agent module must answer) — and these shapes were
//! right, so they were kept.
//!
//! One rule governs the whole module and it is measured, not stylistic:
//! **`serde`'s `rename_all` does not rename fields inside enum variants.** An
//! internally tagged enum needs `rename_all_fields` as well, or a camelCase wire
//! field inside a variant parses as `None` while the code compiles and every
//! non-variant field works. Every such attribute below is load-bearing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A control an agent offers on a live session.
///
/// # Why three variants and not eight
///
/// `sylph.agent.v3` declared eight: `Interrupt`, `Steer`, `Approval`, `Subagent`,
/// `Slash`, `SetConfig`, `Context`, `Fork`. *Measured against `crates/server/src` before
/// this crate existed:* grepping for `Capability::Context|Fork|Approval|Subagent|Slash`
/// outside the declaring module returned **nothing**. Only `Interrupt`, `Steer`, and
/// `SetConfig` were ever read, and each gates exactly one method on [`crate::Session`].
///
/// The five are deleted rather than carried, and they were deleted for three
/// different reasons worth separating, because the reason decides what happens when
/// one is genuinely needed later:
///
/// - **`Approval`, `Subagent`, `Slash` were answered by the surface itself.** Whether
///   a session has slash commands is `commands` on [`SessionCapabilities`]; whether it
///   reports delegations is whether `subagent` events arrive. A second place to say the
///   same thing is one that can disagree with it, and a declaration that contradicts
///   observed behaviour is worse than no declaration. `Approval` additionally existed
///   for the `SERVER_ACCEPTS` handshake negotiation, which went away with the handshake.
/// - **`Context` had no consumer at either end.** It backed the v3 `query` command,
///   whose `Link::query` had zero callers. Context usage is a real thing a user wants,
///   and it comes back as `contextWindow` on the surface plus a `usage` event, not as a
///   query verb.
/// - **`Fork` names an operation this trait does not have.** `codex` really can
///   `exec fork`, and `docs/architecture.md` uses a fork for moving work between
///   environments — but there is no `Adapter::fork`, so the variant would declare a
///   capability nothing can invoke. `AGENTS.md`: a declared-only entry ships a branch
///   that never fires, indistinguishable from one whose condition is merely rare. When
///   fork lands it arrives as a trait method *and* a variant here, in that order.
///
/// # What this is not
///
/// Facts about an agent that are not controls a UI offers do not live here — they are
/// typed fields on [`crate::AgentInfo`]. `dictates_native_id` and `native_fidelity`
/// are the two that would otherwise have been tempting to spell as capabilities, and
/// both decide what the *server* does rather than what a composer renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Can end a turn without ending the session.
    ///
    /// Distinct from a stop signal, which kills the process. Conflating them loses one
    /// of two real intentions: "that is not what I meant" versus "stop everything".
    Interrupt,
    /// Accepts text **into the running turn**, reaching the agent before its next model
    /// call.
    ///
    /// A correction, and the timing is the whole capability: text that arrives after the
    /// turn is a follow-up, which is [`Self::Queue`]. Offered only by a vendor with a native
    /// steer — `codex`'s `turn/steer`, which requires an `expectedTurnId` and refuses a
    /// stale one, while pi's unguarded queue is [`Self::QueueSteering`].
    ///
    /// **`claude` does not have one, and declaring it here was a shipped defect.** Measured
    /// on 2.1.251: `"steer"` occurs zero times as a quoted token in the binary while the six
    /// real control subtypes occur 12–56 times each. Its adapter wrote a second `user`
    /// message to stdin — and `--input-format stream-json` reads stdin through one async
    /// iterator the turn loop awaits, so such a write is not read until the turn ends. The
    /// composer sends a steer whenever a turn is live and the agent claims this, so a user's
    /// correction arrived *after* the thing it was meant to prevent, with every layer
    /// reporting success.
    Steer,
    /// Native priority queue. May cross a turn boundary; acknowledgement proves queue
    /// insertion, not delivery into the currently active turn.
    QueueSteering,
    /// Accepts text **for after the running turn**.
    ///
    /// A follow-up rather than a correction, and a distinct capability rather than a
    /// degraded [`Self::Steer`], because a client renders them differently: a correction is
    /// worth sending mid-turn and a follow-up is worth *showing as queued*. Collapsing them
    /// is what let one vendor claim the wrong word for what it does.
    ///
    /// All three vendors have it and each spells it differently — `codex`'s
    /// `thread/queue/add` (which needs an undocumented `clientUserMessageId`), `pi`'s
    /// `follow_up`, and `claude`'s write to stdin. Note that `pi` also emits `queue_update`
    /// with `steering` and `followUp` arrays: without forwarding it a queued message is
    /// invisible until it runs, so the capability is only half-delivered by the verb.
    Queue,
    /// Accepts a typed config change on a live session.
    SetConfig,
}

/// A spawn-time option an agent declares: model, effort, and whatever else.
///
/// **This crate is the one declaration of the shape.** Three copies of it is one shape
/// that diverges on the first case anyone adds to only one of them, and nothing compares
/// the copies.
///
/// [`CommandSpec`] carries a free-form `arg_hint` rather than a map of typed arguments,
/// because that is what agents actually report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagSpec {
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// Whether a command is something the agent thinks about, or something the session
/// *is*.
///
/// The distinction v2 lacked, and the reason `/model` answered with a paragraph: sent
/// as prompt text to a headless CLI, a request to pick a model can only come back as
/// prose, because there is no picker to open. A `Control` command instead names a
/// [`ConfigOption`] the client renders directly.
///
/// Three protocols reached this split independently, which is what makes it a
/// generalization rather than a guess: ACP has `AvailableCommand` beside
/// `SessionConfigOption`, `codex`'s app-server has no command concept but does have
/// `model/list` and `config/value/write`, and `claude` exposes both.
///
/// Defaults to [`CommandKind::Prompt`], so an agent that says nothing behaves as it
/// did before this field existed.
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
/// **Session state, not agent state.** A project's real command list depends on files
/// the CLI reads at startup, so on a real project this arrives by the dozen where a
/// static descriptor held the one or two an author hand-wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Free-form hint for what follows the command, e.g. `[target]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_hint: Option<String>,
    /// Where it came from: `skill`, `project`, `plugin`, `builtin`. Advisory, used
    /// only for grouping in the UI.
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
    /// Carried because agents ship them and a terminal honours them — `claude` gives
    /// `/usage` the aliases `cost` and `stats`. A menu that ignores them behaves
    /// unlike the terminal it is modelled on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// A subagent definition the agent can delegate to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// One choice within a [`ConfigOption`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigChoice {
    /// **Opaque to the client by contract** — a token to echo back, never to parse.
    ///
    /// That contract is what lets an encoding change without a server or UI change, and it
    /// is why a composite value is delimited with a **space**. Most of `pi`'s model ids
    /// already contain a `/` (`anthropic/claude-opus-5`, `stealth/ox-alpha`), so splitting
    /// on the first or last slash mis-parses the majority of a real catalogue. No provider
    /// and no id contains a space.
    ///
    /// **It must be unique within its option.** `checked` is computed as a filter —
    /// every producer writes `value == current_value` — so the moment two choices
    /// share a value both rows show a checkmark and neither is identifiably the real
    /// one. On that same install `deepseek-v4-pro` and `glm-5.2` are each offered by
    /// two providers, and a bare id made one of each pair *unreachable*, since a
    /// by-id lookup can only answer with one provider. A model's identity is
    /// `(provider, id)`, never the id alone.
    pub value: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Which of the two shapes a [`ConfigOption`] takes.
///
/// `Select` and `Boolean` cover every knob the three supported agents expose:
/// `claude`'s model / effort / permission mode / output style, `pi`'s model /
/// thinking level / queue modes, `codex`'s approval policy. A numeric or free-text
/// kind is a later addition, **not a placeholder now** — an unused variant is a
/// client branch that never renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigKind {
    Select,
    Boolean,
}

/// Something adjustable on a live session.
///
/// The shape is ACP's `SessionConfigOption`, adopted rather than invented.
///
/// # Why a flat struct with a `kind`, where v3 had an internally tagged enum
///
/// `docs/session-protocol.md` § Session config spells it flat, and the flat form is
/// better for the reason the enum was worse: every consumer wanted `id`, `name`,
/// `category`, and `current_value` regardless of kind, so the enum forced a `match`
/// with two identical arms at every one of them — including `ConfigOption::id`, which
/// existed solely to undo the split.
///
/// **The wire form is unchanged by that reshape**, which is why it is safe to land in
/// a step that is only supposed to move types. An internally tagged enum serializes
/// its tag as a sibling of the variant's own fields, so `{"type":"select","id":…}` is
/// what both shapes produce — provided `kind` keeps the wire name `type`, which is
/// also the only reason the Rust field is spelled differently at all (`type` is a
/// keyword). A reader wondering whether this was a silent wire break: it was not, and
/// `a_config_option_wire_form_is_unchanged_by_the_reshape` pins it.
///
/// # Two fields the spec omits and this keeps
///
/// `description` is not in the spec's listing and is in the shipped wire format. The
/// spec is the lossy side there: it is the agent's own explanation of a knob, and a
/// field missing from the type is a field the client never sees — which is exactly how
/// an `AskUserQuestion`'s descriptions were being dropped while the adapter sent them
/// correctly.
///
/// `current_value` is a `serde_json::Value` rather than the spec's `Option<String>`,
/// and this is a deliberate deviation. A `Boolean` option's current value is a
/// boolean; stringifying it would mean the value a client *reads* (`"true"`) is not
/// the value it may *write* (`true`), and [`Self::accepts`] refuses the string form
/// on purpose — a client sending `"true"` is sending something the adapter would have
/// to guess about. One type for both directions is what keeps a read and a write
/// symmetric.
///
/// One consequence of that unification is a real change and an improvement: v3's
/// `Boolean` variant held a bare `bool` that defaulted to `false`, so an agent that
/// never reported a value published `false` — a claim about the session, indistinguishable
/// from a vendor genuinely reporting off. Absent now means absent, which is the same
/// rule `name` follows on an event (`docs/session-protocol.md` § `name` is absent when
/// unknown).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOption {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Advisory grouping — `model`, `effort`, `permission_mode`, or anything an agent
    /// invents — so a client groups by it without having to know the vocabulary.
    /// That openness is the point: the next agent's knobs render without a server or
    /// UI change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Spelled `type` on the wire, because `type` is a Rust keyword and this field is
    /// the discriminant the previous internally tagged enum carried under that name.
    #[serde(rename = "type")]
    pub kind: ConfigKind,
    /// **Read from the vendor's confirmation, never from the request.**
    ///
    /// *Measured on `claude` 2.1.246:* `manual` answers success while confirming
    /// `default` — the vendor normalizes silently, so a picker offering it lands on a
    /// different row. Writing the requested value made the adapter the authority on a
    /// fact only the vendor holds, and the surface then reported a policy the session
    /// was not running under. `pi`'s adapter had the mirror defect: it answered from
    /// a field only one code path populated, so any surface built by another path
    /// reported nothing while the session plainly had a model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_value: Option<serde_json::Value>,
    /// Empty on a `Boolean`, and on a `Select` that declares a free-form field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ConfigChoice>,
}

impl ConfigOption {
    /// Whether this option would accept `value`.
    ///
    /// The server-side validation the spec requires (§ A config value must be unique)
    /// and never declares as a signature. It is here rather than in the server because
    /// the rule is a property of what the *agent* enumerated, and the same check has
    /// to hold for an agent nobody has written yet.
    ///
    /// A `Select` accepts exactly the values it enumerated and a `Boolean` exactly a
    /// JSON boolean. The reason this is checked at all: an unrecognized value reaches
    /// the vendor CLI, and what happens there is undefined — `pi` answered a malformed
    /// model with `Model not found: undefined/undefined`, which surfaced two layers
    /// away as a picker that appeared to do nothing.
    ///
    /// A `Select` that enumerated nothing accepts any string, because that is an agent
    /// declaring a free-form field rather than an empty choice; refusing every value
    /// there would make such an option unusable rather than safe.
    pub fn accepts(&self, value: &serde_json::Value) -> bool {
        match self.kind {
            ConfigKind::Select => {
                if self.options.is_empty() {
                    return value.is_string();
                }
                value
                    .as_str()
                    .is_some_and(|candidate| self.options.iter().any(|c| c.value == candidate))
            }
            ConfigKind::Boolean => value.is_boolean(),
        }
    }
}

/// The live capability surface of an open session.
///
/// Pushed rather than polled: a skill edited on disk or a plugin loading changes what
/// the composer should offer, and a list a client refreshes on its own schedule is
/// wrong for however long that schedule is.
///
/// **An empty surface means "Sylph cannot reach it", never "there is nothing there"**
/// (`docs/architecture.md` § One link, two typed capability facets). Those coincide
/// for a session Sylph launched and are opposite for one running in a terminal, where
/// the process is alive and simply not ours to drive. [`crate::LinkFacets`] is what
/// says which.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

impl SessionCapabilities {
    pub fn config_option(&self, id: &str) -> Option<&ConfigOption> {
        self.config.iter().find(|option| option.id == id)
    }
}

/// A capability update, addressed by facet.
///
/// Every field is an `Option` and **`None` means "not mentioned"**, which is the whole
/// design. Facets: `commands`, `subagents`, `config`, `permissionMode`,
/// `contextWindow`.
///
/// v2 carried the entire surface in one value and assigned it. `claude` emits
/// `system/init` once **per turn**, not once per process, so an adapter answering it
/// with a whole-surface payload carrying only the permission mode erased the project's
/// 46 slash commands on the first exchange of every session — and the symptom appeared
/// two layers away as a composer menu that would not open.
///
/// Merging partial surfaces would have fixed that instance at the same cost and left
/// the hazard: a message about one part of the surface would still be *able* to speak
/// for all of it, and the next adapter would rediscover it. Naming the facet is what
/// makes silence mean silence.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityFacets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<CommandSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<Vec<SubagentSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Vec<ConfigOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
}

impl CapabilityFacets {
    /// A `config`-only update, which is what [`crate::Session::set_config`] answers with.
    ///
    /// Named rather than constructed at the call site so the one path that turns a
    /// refreshed config array into an update cannot accidentally mention a second
    /// facet.
    pub fn config(options: Vec<ConfigOption>) -> Self {
        Self {
            config: Some(options),
            ..Default::default()
        }
    }

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
    ///
    /// # Under the trait, the server owns this merge
    ///
    /// [`crate::SessionMessage::Capabilities`] carries a *partial*, and the server is
    /// what applies it. That is the deliberate inversion of what the v3 transport did:
    /// `Link` cached a `SessionCapabilities` and merged facets inside itself, so the
    /// server only ever saw whole surfaces.
    ///
    /// The reasoning is the same rule as [`crate::Event::stability`] — one fact, one
    /// derivation. With three adapters each merging internally there would be three
    /// implementations of "an unmentioned facet is not changed", so the next adapter
    /// author would rediscover the 46-erased-commands bug rather than inherit the fix;
    /// and the server, which is the authority on session state, would be holding a copy
    /// it did not compute. `docs/session-protocol.md` § Announce every departure states
    /// the general form: one publisher every transition calls, because three call sites
    /// each remembering is three chances to forget.
    ///
    /// This is the only implementation, and every caller is the server: `Link::apply`
    /// for an adapter-pushed update and `Link::set_config` for an accepted setting, both
    /// under the one mutex guarding the merged surface in the server bridge. An
    /// adapter returns facets and never a surface, so it has nothing to merge with.
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

/// Spawn-time options an agent declares, keyed by flag name.
///
/// A `BTreeMap` rather than a `HashMap` so a listing is stable: these are rendered as
/// a settings form, and a control that changes position between reads reads as a bug.
pub type FlagSchema = BTreeMap<String, FlagSpec>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    fn select(id: &str, values: &[&str]) -> ConfigOption {
        ConfigOption {
            id: id.to_string(),
            name: String::new(),
            description: None,
            category: None,
            kind: ConfigKind::Select,
            current_value: None,
            options: values
                .iter()
                .map(|value| ConfigChoice {
                    value: value.to_string(),
                    name: String::new(),
                    description: None,
                })
                .collect(),
        }
    }

    /// The regression this vocabulary exists for.
    ///
    /// A partial update used to be applied by assigning the whole surface, so an
    /// adapter reporting only a permission-mode change also declared that the session
    /// had no slash commands. On a real project that erased 46 of them.
    #[test]
    fn a_facet_update_leaves_the_facets_it_does_not_mention_alone() {
        let mut surface = SessionCapabilities {
            commands: vec![prompt_command("verify")],
            subagents: vec![SubagentSpec {
                name: "Explore".into(),
                description: String::new(),
            }],
            config: vec![],
            permission_mode: Some("default".into()),
            context_window: Some(1_000_000),
        };

        let facets: CapabilityFacets =
            serde_json::from_str(r#"{"permissionMode":"plan"}"#).unwrap();
        facets.apply_to(&mut surface);

        assert_eq!(surface.permission_mode.as_deref(), Some("plan"));
        assert_eq!(
            surface.commands.len(),
            1,
            "commands must survive a mode-only update"
        );
        assert_eq!(surface.subagents.len(), 1);
        assert_eq!(surface.context_window, Some(1_000_000));
    }

    /// An explicitly empty list is a real value: an agent whose plugins were unloaded
    /// genuinely has no commands, and that must stay expressible.
    #[test]
    fn an_explicitly_empty_facet_does_clear_that_facet() {
        let mut surface = SessionCapabilities {
            commands: vec![prompt_command("verify")],
            ..Default::default()
        };
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
        assert!(
            !named.is_empty(),
            "an empty list is still a statement about commands"
        );
    }

    /// `set_config` answers with a refreshed config array, and that answer must not be
    /// able to speak about anything else.
    #[test]
    fn a_config_refresh_mentions_only_the_config_facet() {
        let facets = CapabilityFacets::config(vec![select("model", &["a"])]);
        assert!(facets.config.is_some());
        assert!(facets.commands.is_none());
        assert!(facets.permission_mode.is_none());
    }

    /// `currentValue` lives on a struct field rather than inside a variant now, but
    /// the camelCase spelling is still the thing that broke before, so it is pinned.
    #[test]
    fn a_config_option_parses_its_camel_case_current_value() {
        let option: ConfigOption = serde_json::from_str(
            r#"{"id":"model","name":"Model","type":"select","category":"model",
                "currentValue":"sonnet",
                "options":[{"value":"sonnet","name":"Sonnet 5"}]}"#,
        )
        .unwrap();
        assert_eq!(option.current_value, Some(json!("sonnet")));
        assert_eq!(option.options[0].name, "Sonnet 5");
    }

    /// The reshape from an internally tagged enum to a flat struct must not have moved
    /// a byte on the wire, because three Python adapters and the frontend both spell
    /// the discriminant `type` at the top level beside the option's own fields.
    ///
    /// Asserted in both directions: a parse-only test would pass against a producer
    /// that had started emitting `kind`.
    #[test]
    fn a_config_option_wire_form_is_unchanged_by_the_reshape() {
        let mut option = select("model", &["a"]);
        option.current_value = Some(json!("a"));
        let wire = serde_json::to_value(&option).unwrap();
        assert_eq!(wire["type"], "select");
        assert!(
            wire.get("kind").is_none(),
            "the Rust field name must not leak"
        );
        assert_eq!(wire["id"], "model", "the tag is a sibling, not a wrapper");
        assert_eq!(
            serde_json::from_value::<ConfigOption>(wire).unwrap(),
            option
        );
    }

    /// A boolean's current value is a boolean. The spec spells this field
    /// `Option<String>`; stringifying it would make the value a client reads differ
    /// from the value it may write, which `accepts` refuses on purpose.
    #[test]
    fn a_boolean_option_reports_a_boolean_current_value() {
        let option: ConfigOption =
            serde_json::from_str(r#"{"id":"verbose","type":"boolean","currentValue":true}"#)
                .unwrap();
        assert_eq!(option.current_value, Some(json!(true)));
        assert!(option.accepts(&json!(false)));
        assert!(!option.accepts(&json!("true")));
        assert!(!option.accepts(&json!(1)));
    }

    /// An agent that reported no value must publish absence, not `false`. v3's bare
    /// `bool` could not express this: it defaulted to `false`, which is a claim about
    /// the session indistinguishable from the vendor reporting off.
    #[test]
    fn a_boolean_that_reported_nothing_says_nothing() {
        let option: ConfigOption =
            serde_json::from_str(r#"{"id":"verbose","type":"boolean"}"#).unwrap();
        assert_eq!(option.current_value, None);
        let wire = serde_json::to_value(&option).unwrap();
        assert!(wire.get("currentValue").is_none());
    }

    #[test]
    fn a_select_accepts_only_the_values_it_enumerated() {
        let option = select("model", &["a", "b"]);
        assert!(option.accepts(&json!("a")));
        assert!(option.accepts(&json!("b")));
        assert!(!option.accepts(&json!("c")));
        // Not a string at all: a client sending the wrong JSON type is as wrong as one
        // sending an unknown name, and for the same reason.
        assert!(!option.accepts(&json!(true)));
        assert!(!option.accepts(&json!(null)));
    }

    /// An enumerated-nothing `select` is a free-form field, not an empty choice.
    #[test]
    fn a_select_with_no_choices_accepts_any_string() {
        let option = select("cwd", &[]);
        assert!(option.accepts(&json!("/tmp/anywhere")));
        assert!(!option.accepts(&json!(7)));
    }

    /// Two providers offering the same model are two distinct values.
    ///
    /// *Measured on `pi` 0.84.3:* `deepseek-v4-pro` and `glm-5.2` are each offered by
    /// two providers, so a bare id was not a unique value — which is what made two rows
    /// read as current at once. Both spellings must survive validation independently,
    /// or one provider becomes unreachable.
    #[test]
    fn provider_qualified_values_are_each_accepted_on_their_own() {
        let option = select("model", &["ikun-glm glm-5.2", "huaweicloud glm-5.2"]);
        assert!(option.accepts(&json!("ikun-glm glm-5.2")));
        assert!(option.accepts(&json!("huaweicloud glm-5.2")));
        // The bare id is now nobody's value, so it must not slip through.
        assert!(!option.accepts(&json!("glm-5.2")));
    }

    #[test]
    fn capabilities_default_to_empty_so_a_minimal_agent_is_usable() {
        let caps: SessionCapabilities = serde_json::from_str("{}").unwrap();
        assert!(caps.commands.is_empty());
        assert!(caps.config.is_empty());
        assert!(caps.permission_mode.is_none());
    }

    /// Absent `kind` means prompt, so an agent that reports nothing behaves as it did
    /// before the field existed.
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
}
