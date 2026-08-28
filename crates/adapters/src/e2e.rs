//! The deterministic E2E agent.
//!
//! Real agents are non-deterministic and need credentials, so the suite runs against
//! this instead and still exercises the production path end to end: a session held open
//! across turns, a capability surface, streamed events, and requests the UI has to
//! answer.
//!
//! # Why this is compiled in rather than a subprocess
//!
//! It replaced `frontend/e2e/fixtures/adapters/e2e/adapter.py`, which was found by
//! agent discovery looking for `adapter.sh`. Discovery is deleted
//! (`docs/session-protocol.md` § What changed), so a fixture that lived on disk could
//! not be found at all — and the failure mode was the expensive one: the suite still
//! ran, every spec that created a session failed against an agent that did not exist,
//! and the failures read as product bugs. `AGENTS.md` records that happening twice.
//!
//! Behind the `e2e` cargo feature, so a release build cannot offer it.
//!
//! # The sizes and timings here are the test, not decoration
//!
//! Four separate times in this repo's history a fixture was too small or too fast to
//! observe the defect a spec existed to catch, and the suite stayed green through all
//! four. Every number below is load-bearing and its reason is stated at it. Before
//! changing one, find the spec that depends on it.

use std::collections::BTreeMap;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use sylph_protocol::{
    Adapter, AgentInfo, Capability, ConfigChoice, ConfigKind, ConfigOption, EventStream, Fidelity,
    FlagSpec, NativeId, NativeSummary, RunSpec, Session, SessionCapabilities, StoreFilter,
};

/// The name this adapter answers to.
///
/// Not a real CLI, so [`E2eAdapter::is_installed`] is unconditionally true: availability
/// for every other agent is "is the vendor binary on `PATH`", and this one is the binary.
pub const AGENT_NAME: &str = "e2e";

/// A long model catalogue, because the threshold between "a menu" and "a searchable
/// list" is a real behaviour and nothing could observe it while every fixture option had
/// two choices.
///
/// Seventeen is well above the client's search threshold of 8 and inside the range real
/// agents report — `pi` declares 372 across providers, `claude` around a dozen. The ids
/// are deliberately long enough to be the kind that once widened a `<select>` past the
/// right edge of a phone.
///
/// **Two entries share a display name on purpose.** Measured on `pi` 0.84.3: two
/// providers each offer "Claude Opus 5", and a producer writing
/// `checked: value == current` marks *every* match — so a non-unique value put a
/// checkmark on two rows and made one of the pair unreachable by id. The values here are
/// provider-qualified for the same reason `pi`'s adapter's are, and the pair exists so a
/// spec can prove exactly one row reads as current.
///
/// **`refused` is rejected by `set_config` on purpose**, so the client's failure path is
/// observable rather than merely reasoned about.
pub const CATALOGUE: &[(&str, &str, &str)] = &[
    ("opus-5-1m", "Opus 5 (1M context)", "Anthropic"),
    ("opus-5", "Opus 5", "Anthropic"),
    ("sonnet-5", "Sonnet 5", "Anthropic"),
    ("haiku-4-5", "Haiku 4.5", "Anthropic"),
    ("fable-5", "Fable 5", "Anthropic"),
    ("gpt-5-codex-preview", "GPT-5 Codex Preview", "OpenAI"),
    ("gpt-5-mini", "GPT-5 Mini", "OpenAI"),
    ("o5-reasoning", "o5 Reasoning", "OpenAI"),
    ("kimi-k2-thinking", "Kimi K2 Thinking", "Moonshot"),
    ("kimi-k2", "Kimi K2", "Moonshot"),
    ("llama-4-405b-instruct", "Llama 4 405B Instruct", "Meta"),
    ("mistral-large-3", "Mistral Large 3", "Mistral"),
    ("qwen-3-max", "Qwen 3 Max", "Alibaba"),
    ("deepseek-v4", "DeepSeek V4", "DeepSeek"),
    // The duplicated display name, provider-qualified.
    ("ikun glm-5.2", "GLM-5.2", "ikun-glm"),
    ("huaweicloud glm-5.2", "GLM-5.2", "huaweicloud"),
    // Refused below.
    ("refused", "Unavailable Model", "Refuses to be set"),
];

/// The value `set_config` refuses, with `INVALID_VALUE` rather than `UNSUPPORTED_FIELD`.
///
/// **The distinction is the whole point of the fixture carrying this at all.** The option
/// exists and one value is rejected, which is `INVALID_VALUE`; the server classifies on
/// that code and keeps the control usable, while `UNSUPPORTED_FIELD` justifies disabling
/// it. The Python fixture used the wrong one, so the suite exercised "no such option"
/// while claiming to cover a refused *value*, and the branch that matters had no
/// end-to-end coverage (`docs/session-protocol.md` § A refused set).
pub const REFUSED_VALUE: &str = "refused";

/// The scenario a prompt selects, by substring.
///
/// One fixture serves the whole suite, and the substrings are what the specs type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// A long transcript, for measuring render cost. Completed blocks only, so every one
    /// is journalled and survives a reload — which is what makes the measurement
    /// reproducible rather than dependent on stream timing.
    Bulk,
    /// A read, an edit carrying a real diff, and a failing command: the three renderings
    /// whose expansion policy differs.
    Tools,
    /// A request that blocks until answered — the inversion v1 could not express, and
    /// the reason the fixture had to become a duplex adapter at all.
    Approval,
    /// An approval that forbids a lasting grant. Offering "always" here would write a
    /// rule broader than the ask, so a client must not show it.
    Narrow,
    /// A declared form with two fields, because one could not observe that two radio
    /// groups stay independent — a single shared `name` would make answering the second
    /// clear the first.
    Question,
    /// A text form carrying a prefill: the kind whose entire purpose is free text and
    /// which, before a form could be declared, rendered as a read-only dump of its own
    /// placeholder.
    Elicit,
    /// Concurrency, depth 3, a buried failure, and one left open. A flat subagent list
    /// passes at depth 1 and breaks at depth 2.
    Delegate,
    /// Work dispatched into the background, reporting **after** the turn settles.
    Unprompted,
    /// Delegation that is never *declared* — the shape every delegating tool outside
    /// `claude`'s built-in `Task` actually has, discovered only because children name
    /// their host (`docs/session-protocol.md` § Promotion).
    Promote,
    /// Deliberately slow, so "did the UI paint mid-turn" is observable rather than
    /// inferred.
    Paced,
    /// A wide delegation with genuinely different durations — the one shape no other
    /// scenario can produce.
    Fanout,
    /// Anything else: a streamed reply.
    Reply,
}

impl Scenario {
    /// Which scenario a prompt asks for.
    ///
    /// Substring rather than equality because the specs send real sentences, and first
    /// match wins in the order the Python fixture checked them — `promote` before
    /// `paced` matters only if a prompt contains both, but keeping the order identical
    /// means a spec that worked against the Python one still works.
    pub fn of(text: &str) -> Self {
        for (needle, scenario) in [
            ("bulk", Self::Bulk),
            ("tools", Self::Tools),
            ("approval", Self::Approval),
            ("narrow", Self::Narrow),
            ("question", Self::Question),
            ("elicit", Self::Elicit),
            ("delegate", Self::Delegate),
            ("unprompted", Self::Unprompted),
            ("promote", Self::Promote),
            ("paced", Self::Paced),
            ("fanout", Self::Fanout),
        ] {
            if text.contains(needle) {
                return scenario;
            }
        }
        Self::Reply
    }
}

/// The deterministic test adapter.
#[derive(Debug, Clone, Default)]
pub struct E2eAdapter;

impl E2eAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Always available: this adapter *is* the binary, so there is nothing to find on
    /// `PATH`. The `e2e` cargo feature is what gates it.
    pub fn is_installed() -> bool {
        true
    }

    /// The spawn-time flags, which are a different list from the live config.
    ///
    /// Long for the same reason the catalogue is: the settings panel's per-agent default
    /// is a second surface with the same scroll-and-filter problem, and a two-item list
    /// could not observe it.
    fn flags() -> BTreeMap<String, FlagSpec> {
        let mut choices = vec!["fast".to_string(), "thorough".to_string()];
        choices.extend(CATALOGUE.iter().map(|(value, _, _)| value.to_string()));
        BTreeMap::from([(
            "model".to_string(),
            FlagSpec {
                choices,
                description: "Model tier".to_string(),
            },
        )])
    }

    /// The live config surface.
    ///
    /// Two options in one category on purpose: below the client's threshold an option is
    /// a menu of radios, above it a searchable listbox, and those are different ARIA
    /// contracts rather than different widths. Both land on the composer's control row,
    /// which is what lets one screen assert both patterns.
    fn config() -> Vec<ConfigOption> {
        vec![
            ConfigOption {
                id: "model".into(),
                name: "Model".into(),
                category: Some("model".into()),
                description: None,
                kind: ConfigKind::Select,
                current_value: Some("fast".into()),
                options: vec![
                    ConfigChoice {
                        value: "fast".into(),
                        name: "Fast".into(),
                        description: None,
                    },
                    ConfigChoice {
                        value: "thorough".into(),
                        name: "Thorough".into(),
                        description: None,
                    },
                ],
            },
            ConfigOption {
                id: "provider".into(),
                name: "Provider model".into(),
                category: Some("model".into()),
                description: None,
                kind: ConfigKind::Select,
                current_value: Some("sonnet-5".into()),
                options: CATALOGUE
                    .iter()
                    .map(|(value, name, provider)| ConfigChoice {
                        value: (*value).into(),
                        name: (*name).into(),
                        description: Some((*provider).into()),
                    })
                    .collect(),
            },
        ]
    }
}

#[async_trait]
impl Adapter for E2eAdapter {
    async fn probe(&self) -> Result<AgentInfo> {
        Ok(AgentInfo {
            name: AGENT_NAME.to_string(),
            version: Some("3.0.0".to_string()),
            flags: Self::flags(),
            supports: vec![
                Capability::Interrupt,
                Capability::Steer,
                Capability::SetConfig,
            ],
            dictates_native_id: true,
            native_fidelity: Fidelity::Stream,
        })
    }

    async fn run(&self, _spec: RunSpec) -> Result<Box<dyn Session>> {
        Err(anyhow!(
            "the e2e session is not wired yet: its scenarios land with the rest of this \
             module"
        ))
    }

    async fn adopt(&self, _id: &NativeId) -> Result<EventStream> {
        Err(anyhow!(
            "the e2e adapter has no native store; adopt is for reading a vendor's files"
        ))
    }

    async fn watch(&self, _id: &NativeId) -> Result<EventStream> {
        Err(anyhow!(
            "the e2e adapter has no native store; watch is for following a vendor's files"
        ))
    }

    async fn enumerate(&self, _filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
        // Not an error: a machine with this adapter and no native sessions is the normal
        // case, and an empty listing is the truthful answer.
        Ok(Vec::new())
    }
}

/// State a session carries across turns.
///
/// Held behind a `Mutex` rather than rebuilt per turn because a config set must be
/// visible to the *next* turn: the whole point of a session outliving a turn is that
/// vendor state persists, and a fixture that forgot would make every set look refused.
#[derive(Debug)]
struct Surface {
    config: Vec<ConfigOption>,
    turn: usize,
}

/// A live E2E session.
pub struct E2eSession {
    native_id: NativeId,
    surface: Mutex<Surface>,
}

impl E2eSession {
    fn new(native_id: NativeId) -> Self {
        Self {
            native_id,
            surface: Mutex::new(Surface {
                config: E2eAdapter::config(),
                turn: 0,
            }),
        }
    }

    /// The capability surface, read from what this session currently holds.
    fn capability_surface(&self) -> SessionCapabilities {
        let surface = self.surface.lock().expect("the fixture is single-threaded");
        SessionCapabilities {
            config: surface.config.clone(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue's size is what makes a searchable list observable.
    #[test]
    fn the_catalogue_is_long_enough_to_need_searching() {
        assert!(
            CATALOGUE.len() > 8,
            "the client switches from a menu to a searchable list above 8; a shorter \
             catalogue cannot observe either behaviour"
        );
        assert_eq!(CATALOGUE.len(), 17);
    }

    /// Two providers, one display name, distinct values.
    ///
    /// The measured `pi` shape. Without it, a producer computing `checked` as a filter
    /// would put a checkmark on two rows and nothing could see it.
    #[test]
    fn one_display_name_is_offered_by_two_providers_under_distinct_values() {
        let glm: Vec<_> = CATALOGUE.iter().filter(|(_, name, _)| *name == "GLM-5.2").collect();
        assert_eq!(glm.len(), 2, "the duplicated display name must survive");

        let values: std::collections::BTreeSet<_> = glm.iter().map(|(value, _, _)| *value).collect();
        assert_eq!(values.len(), 2, "and each must be reachable by its own value");

        // Every value in the catalogue is unique, or `checked` marks more than one row.
        let all: std::collections::BTreeSet<_> =
            CATALOGUE.iter().map(|(value, _, _)| *value).collect();
        assert_eq!(all.len(), CATALOGUE.len());
    }

    /// Exactly one choice can read as current, because `checked` is derived.
    ///
    /// `ConfigChoice` deliberately carries no `checked` field — every producer computes
    /// `value == current_value`, which is a *filter*, so uniqueness of the value is what
    /// guarantees one current row. That makes this a test about the fixture's data rather
    /// than about a flag it sets, which is the stronger place for it.
    #[test]
    fn exactly_one_choice_matches_each_options_current_value() {
        for option in E2eAdapter::config() {
            let current = option
                .current_value
                .as_ref()
                .and_then(|value| value.as_str())
                .expect("the fixture sets a current value on every option")
                .to_string();
            let matches = option
                .options
                .iter()
                .filter(|choice| choice.value == current)
                .count();
            assert_eq!(
                matches, 1,
                "option {} must have exactly one choice matching its current value",
                option.id
            );
        }
    }

    /// Scenario selection, including that the fallthrough is a reply rather than an error.
    #[test]
    fn a_prompt_selects_its_scenario_by_substring() {
        assert_eq!(Scenario::of("please run the tools scenario"), Scenario::Tools);
        assert_eq!(Scenario::of("delegate this"), Scenario::Delegate);
        assert_eq!(Scenario::of("paced"), Scenario::Paced);
        assert_eq!(Scenario::of("fanout now"), Scenario::Fanout);
        assert_eq!(Scenario::of("hello"), Scenario::Reply);
    }
}
