//! Whether the environment this envagent inherited has redirected `claude`'s auth.
//!
//! `docs/agents.md` § A host-managed auth switch inherited from another agent session is
//! the governing spec, and holds the measurements every constant here was derived from.
//!
//! # The failure this exists to make loud
//!
//! A vendor process inherits the envagent's environment ([`crate::process::Spawn::env`]).
//! When an envagent is started from *inside* another coding-agent session, it inherits that
//! session's `CLAUDE_CODE_*` variables — and two of them tell the CLI that the **host**
//! supplies credentials instead of its own keychain. With no host credential in the
//! environment, every session Sylph starts answers `Not logged in · Please run /login`
//! while `claude auth status` in the developer's own terminal reports `loggedIn: true`.
//!
//! That is the expensive shape `AGENTS.md` names: not a crash, but a plausible answer. The
//! CLI is behaving exactly as asked, the transcript records a real refusal, and nothing in
//! the chain between the two says the environment was the cause.
//!
//! # One cause with two symptoms, so the endpoint is part of the same judgement
//!
//! The same inherited environment also carries [`BASE_URL`], pointing at whatever endpoint
//! that host talks to. Removing only the two switches restores the keychain token and
//! leaves it aimed there — and a relay issues its own tokens, so it rejects that one. The
//! session then fails with `401 Invalid token` instead of `Not logged in`: a *different*
//! symptom of one cause, which reads as a regression in the repair rather than as progress.
//!
//! So the broken state is the whole conjunction, and neutralizing it means all three
//! variables. What this must not become is a filter on [`BASE_URL`]: measured, a redirected
//! endpoint *with* a credential is a working relay deployment, and a redirected endpoint
//! with no switch at all is an operator's own gateway. Only the conjunction is knowably
//! broken.
//!
//! # Why the pointer's *name* is not a credential source
//!
//! `CLAUDE_CODE_HOST_AUTH_ENV_VAR` reads as "the token is in the variable I name", so the
//! obvious repair is to look up that variable. **Measured on 2.1.267, it is not read as
//! one.** Setting the pointer to `MY_HOST_TOKEN` *and* setting `MY_HOST_TOKEN` still
//! reports `loggedIn: false`, while an unrelated pointer name plus a real
//! `ANTHROPIC_API_KEY` reports `loggedIn: true`. So the pointer's only measured effect is
//! to **disable the keychain**; what rescues the CLI is one of the credential variables it
//! reads regardless of the pointer. A repair built on the pointer's name would have been a
//! check that cannot fail.
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{debug, error};

/// Names the variable a host claims to supply auth in. Any non-empty value disables the
/// CLI's own keychain oauth, whatever it names (see this module's header).
pub const HOST_AUTH_POINTER: &str = "CLAUDE_CODE_HOST_AUTH_ENV_VAR";

/// Declares that a host manages the provider credential. Disables keychain oauth when
/// truthy.
pub const PROVIDER_MANAGED_BY_HOST: &str = "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST";

/// The two switches that disable keychain oauth.
pub const KEYCHAIN_SWITCHES: &[&str] = &[HOST_AUTH_POINTER, PROVIDER_MANAGED_BY_HOST];

/// Redirects the API endpoint away from Anthropic's own.
///
/// **Not a switch and not a credential**, so it never makes an environment broken by
/// itself; it decides whether removing the switches is *enough*. *Measured on 2.1.267 with
/// both switches and no credential:* removing the switches alone leaves `-p` failing with
/// `401 Invalid token`, and removing this as well makes the same command exit 0.
///
/// The provider-specific endpoint variables (`ANTHROPIC_VERTEX_BASE_URL`,
/// `ANTHROPIC_BEDROCK_BASE_URL`, and the rest) are deliberately absent. Each is read only
/// by the third-party provider that names it, and selecting such a provider is already
/// [`HostAuth::Managed`] — so an environment where one of them could matter is one this
/// module leaves untouched anyway.
pub const BASE_URL: &str = "ANTHROPIC_BASE_URL";

/// Credential variables the CLI reads directly.
///
/// *Measured:* with either switch set, a non-empty `ANTHROPIC_API_KEY` reports
/// `authMethod: "api_key"` and a non-empty `ANTHROPIC_AUTH_TOKEN` reports
/// `authMethod: "oauth_token"` — so a switch plus one of these is a **working**
/// host-managed configuration, not a broken one.
const HOST_CREDENTIALS: &[&str] = &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"];

/// Provider selectors that make the switches meaningful without any Anthropic token.
///
/// *Measured:* `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST=1` with `CLAUDE_CODE_USE_BEDROCK=1`
/// reports `loggedIn: true, authMethod: "third_party", apiProvider: "bedrock"`, and the
/// same holds for `vertex` and `foundry`. This is the legitimate deployment a blanket
/// strip would have broken.
const THIRD_PARTY_PROVIDERS: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// Other routes by which a host may intend to supply a credential.
///
/// **Held as evidence of intent, not as a credential that works.** *Measured:* a populated
/// `CLAUDE_CODE_HOST_CREDS_FILE` did **not** rescue either switch — the CLI still reported
/// `loggedIn: false`. The two file-descriptor routes were not reduced to a measurement,
/// because passing a live descriptor is not something a probe can fake convincingly.
///
/// They are still consulted here, and that asymmetry is deliberate: this module only
/// removes a variable whose state it has shown to be **unambiguously** broken, and an
/// operator who set one of these is configuring host-managed auth on purpose. Reporting
/// such a case and leaving it intact is right even where it does not work, because the
/// fix belongs to whoever wrote the deployment.
const HOST_CREDENTIAL_ROUTES: &[&str] = &[
    "CLAUDE_CODE_HOST_CREDS_FILE",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    "CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR",
];

/// What the inherited environment has done to this vendor's authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAuth {
    /// Neither switch is set. The CLI reads its own keychain, which is the normal case.
    Untouched,
    /// A switch is set **and** something can answer for it: a credential variable, a
    /// third-party provider, or an explicit host credential route. Left exactly as found.
    Managed {
        /// Why this is believed to be deliberate, so a report can say so.
        because: String,
    },
    /// A switch is set and **nothing** can answer for it. The keychain is disabled and no
    /// credential exists, so every session refuses to run.
    Broken {
        /// The switches to remove, in the order they are named in [`KEYCHAIN_SWITCHES`].
        switches: Vec<String>,
        /// [`BASE_URL`], when it is set and so must be removed too.
        ///
        /// Held apart from `switches` rather than appended to one list, because the two are
        /// broken for different reasons and the report has to say which: the switches
        /// disable the login, this redirects where the restored login is sent. A single
        /// list would force one sentence to cover both, and the 401 case is precisely the
        /// one a reader arrives at needing to know the endpoint moved.
        endpoint: Option<String>,
    },
}

/// Classify one environment, reading it through `lookup`.
///
/// Pure, and takes a lookup rather than reading the process environment, because
/// `std::env::set_var` is process-global and unsafe in a threaded test binary — so cases
/// that must differ per test cannot each mutate the real environment. This is the same
/// reasoning `crate::adapters::AdapterSource` records for `PATH`.
///
/// An **empty** value is not set. *Measured:* `CLAUDE_CODE_HOST_AUTH_ENV_VAR=` leaves the
/// keychain working, so treating the empty string as a switch would report a fault where
/// the CLI has none.
pub fn classify(lookup: impl Fn(&str) -> Option<String>) -> HostAuth {
    let present = |name: &str| lookup(name).is_some_and(|value| !value.is_empty());

    let mut switches = Vec::new();
    if present(HOST_AUTH_POINTER) {
        switches.push(HOST_AUTH_POINTER.to_string());
    }
    // Truthiness rather than presence, and measured: `0`, `false`, `no`, `off`, `2`, and
    // `xyz` all leave the keychain working, while `1`, `true`, `yes`, and `on` disable it
    // in any case. So a `=0` is an operator switching this *off*, and reporting it as a
    // fault would be a diagnostic that cries wolf on a correct configuration.
    if lookup(PROVIDER_MANAGED_BY_HOST).is_some_and(|value| is_truthy(&value)) {
        switches.push(PROVIDER_MANAGED_BY_HOST.to_string());
    }
    if switches.is_empty() {
        return HostAuth::Untouched;
    }

    if let Some(name) = HOST_CREDENTIALS.iter().find(|name| present(name)) {
        return HostAuth::Managed {
            because: format!("{name} carries a credential"),
        };
    }
    if let Some(name) = THIRD_PARTY_PROVIDERS
        .iter()
        .find(|name| lookup(name).is_some_and(|value| is_truthy(&value)))
    {
        return HostAuth::Managed {
            because: format!("{name} selects a third-party provider"),
        };
    }
    if let Some(name) = HOST_CREDENTIAL_ROUTES.iter().find(|name| present(name)) {
        return HostAuth::Managed {
            because: format!("{name} names a host credential route"),
        };
    }

    // Reached only past every `Managed` arm, which is the whole reason the endpoint may be
    // judged at all. On this path the keychain is disabled, no credential answers for it,
    // and no provider is selected — so a redirected endpoint cannot be an operator's
    // working relay, and removing the switches without it would only change which error
    // the session fails with.
    HostAuth::Broken {
        switches,
        endpoint: Some(BASE_URL.to_string()).filter(|name| present(name)),
    }
}

/// Whether the CLI reads this value as on.
///
/// *Measured on 2.1.267* against `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST`: `1`, `true`,
/// `TRUE`, `True`, `yes`, `YES`, `on`, and `ON` disable the keychain; `0`, `false`, `no`,
/// `off`, the empty string, `2`, and `xyz` leave it working. So this is a fixed set rather
/// than "any non-empty value", which would have misread `=0` as on.
fn is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Reported once per envagent run, per the degraded-dependency rule in `AGENTS.md`:
/// one fact about the run, not one per spawn. A session-per-minute workload would
/// otherwise repeat the same paragraph forever.
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Decide what to remove from a `claude` spawn's inherited environment, and say so.
///
/// Returns the variable names to remove — empty for every case except an unambiguously
/// broken one. Reads the real process environment, so it belongs on the spawn path rather
/// than in a test; [`classify`] is the testable half.
///
/// # Why this strips at all, and why only here
///
/// `AGENTS.md` warns against a silent mutation, and the steer for this change was to prefer
/// reporting over mutating. Removal is taken only for [`HostAuth::Broken`], where the state
/// is measured to be unusable rather than merely unusual: the keychain is disabled, no
/// credential variable is set, no third-party provider is selected, and **no repair exists
/// inside the session** — measured, neither `CLAUDE_CODE_HOST_CREDS_FILE` nor an
/// `apiKeyHelper` in `--settings` rescues it. There is no configuration this state serves,
/// so leaving it intact preserves nothing and costs every session.
///
/// **Rejected: reporting only, and leaving the environment alone.** It is the safer default
/// and it is what a genuinely ambiguous case gets — [`HostAuth::Managed`] is returned
/// untouched even where the measurement says it will not work, because an operator wrote it
/// on purpose. It was rejected *for the broken case* because the report cannot repair it:
/// the envagent is long-lived and the inheritance is fixed at its launch, so every session
/// until someone restarts it from a clean terminal fails, having been told why.
///
/// **Rejected: `env_clear()` on every spawn.** It removes the whole class rather than the
/// two variables measured to cause it, and a vendor CLI needs `HOME`, `PATH`, and its own
/// credentials to work at all — so the blast radius is every unrelated variable a
/// deployment legitimately relies on, in exchange for a fault this names precisely.
///
/// **Rejected: repairing by reading the variable the pointer names.** Measured not to be
/// read as a credential at all (this module's header), so it would have been a check that
/// cannot fail.
///
/// **Rejected: removing [`BASE_URL`] whenever it is set.** It is the one variable here that
/// is *ordinarily* legitimate — a relay or gateway deployment is configured with it — so a
/// blanket removal would silently route an operator's traffic back to Anthropic's endpoint,
/// which is the failure mode this module exists to avoid rather than a smaller version of
/// it. It is removed only on the [`HostAuth::Broken`] path, where every credential and
/// provider check has already found nothing.
///
/// **Rejected: treating [`BASE_URL`] as a switch of its own.** A redirected endpoint with no
/// keychain switch is an operator pointing the CLI somewhere on purpose, and the CLI's own
/// login works fine against a relay that accepts it. Classifying it as a fault would report
/// one where the measurement shows none, which is the `=0` mistake in another spelling.
///
/// The strip is never silent: it is reported at `error` with the variables it removed.
pub fn sanitize_inherited_env() -> Vec<String> {
    sanitize(&classify(|name| std::env::var(name).ok()))
}

/// [`sanitize_inherited_env`] over an already-classified environment, so the report and the
/// removal are one implementation a test can drive.
fn sanitize(auth: &HostAuth) -> Vec<String> {
    match auth {
        HostAuth::Untouched => Vec::new(),
        HostAuth::Managed { because } => {
            report(format!(
                "`claude` host-managed authentication is active in this envagent's \
                 environment ({because}); its own keychain login is disabled. Leaving it \
                 as configured."
            ));
            Vec::new()
        }
        HostAuth::Broken { switches, endpoint } => {
            // One report covering both halves, per the degraded-dependency rule: this is one
            // fact about the run. The endpoint clause is separate prose because it fails for
            // its own reason, and naming only the switches would leave the 401 unexplained.
            let endpoint_clause = match endpoint {
                Some(name) => format!(
                    " Also removing {name}, which redirects the API endpoint: with the \
                     switches gone the CLI's own keychain login is restored, and sending it \
                     to that endpoint fails with \"401 Invalid token\" instead."
                ),
                None => String::new(),
            };
            report(format!(
                "Removing {} from `claude` spawns: they disable the CLI's keychain login \
                 while no host credential is present, so every session would answer \
                 \"Not logged in\".{endpoint_clause} This is what an envagent started from \
                 inside another agent session inherits; restart it from a clean terminal to \
                 stop inheriting it (`docs/agents.md` § A host-managed auth switch \
                 inherited from another agent session).",
                switches.join(" and ")
            ));
            switches
                .iter()
                .cloned()
                .chain(endpoint.clone())
                .collect::<Vec<_>>()
        }
    }
}

/// Once at `error`, then at `debug`. See [`REPORTED`].
fn report(message: String) {
    if REPORTED.swap(true, Ordering::SeqCst) {
        debug!("{message}");
    } else {
        error!("{message}");
    }
}

/// A lookup over a fixed table, for a test that must not mutate the process environment.
#[cfg(test)]
fn table(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let held: std::collections::BTreeMap<String, String> = pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    move |name: &str| held.get(name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clean environment is left alone, so nothing is removed from a normal spawn.
    #[test]
    fn an_environment_with_no_host_switch_is_untouched() {
        assert_eq!(classify(table(&[])), HostAuth::Untouched);
        // The credential variables alone are not a switch: measured, `ANTHROPIC_API_KEY`
        // with neither switch set still reports the keychain's own oauth token.
        assert_eq!(
            classify(table(&[
                ("ANTHROPIC_API_KEY", "sk-ant-x"),
                ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ])),
            HostAuth::Untouched
        );
        assert!(sanitize(&HostAuth::Untouched).is_empty());
    }

    /// **A switch with no credential is reported as broken and stripped.**
    ///
    /// This is the measured incident: an envagent launched from inside another agent
    /// session inherits the pointer, and every `claude` session then answers
    /// `Not logged in · Please run /login`.
    #[test]
    fn a_switch_with_no_credential_is_broken_and_its_switches_are_stripped() {
        for switch in [HOST_AUTH_POINTER, PROVIDER_MANAGED_BY_HOST] {
            let value = if switch == HOST_AUTH_POINTER {
                "ANTHROPIC_AUTH_TOKEN"
            } else {
                "1"
            };
            let auth = classify(table(&[(switch, value)]));
            assert_eq!(
                auth,
                HostAuth::Broken {
                    switches: vec![switch.to_string()],
                    endpoint: None,
                },
                "{switch} alone disables the keychain with nothing to answer for it"
            );
            assert_eq!(
                sanitize(&auth),
                vec![switch.to_string()],
                "{switch} must be removed from the spawn"
            );
        }
    }

    /// Both switches at once are both removed, not just the first found.
    #[test]
    fn both_switches_are_stripped_when_both_are_set() {
        let auth = classify(table(&[
            (HOST_AUTH_POINTER, "ANTHROPIC_AUTH_TOKEN"),
            (PROVIDER_MANAGED_BY_HOST, "1"),
        ]));
        assert_eq!(
            sanitize(&auth),
            vec![
                HOST_AUTH_POINTER.to_string(),
                PROVIDER_MANAGED_BY_HOST.to_string(),
            ]
        );
    }

    /// **A legitimate host-managed deployment is left exactly as configured.**
    ///
    /// The cost of getting this wrong is the opposite one: stripping here breaks an
    /// operator's working configuration, and it breaks it silently because the CLI would
    /// then succeed against the *wrong* credential.
    #[test]
    fn a_host_managed_configuration_with_a_credential_is_left_alone() {
        for (name, value) in [
            ("ANTHROPIC_AUTH_TOKEN", "sk-host-token"),
            ("ANTHROPIC_API_KEY", "sk-ant-key"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("CLAUDE_CODE_USE_VERTEX", "1"),
            ("CLAUDE_CODE_USE_FOUNDRY", "1"),
            ("CLAUDE_CODE_HOST_CREDS_FILE", "/run/creds.json"),
            ("CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR", "3"),
            ("CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR", "4"),
        ] {
            let auth = classify(table(&[
                (HOST_AUTH_POINTER, "ANTHROPIC_AUTH_TOKEN"),
                (PROVIDER_MANAGED_BY_HOST, "1"),
                (name, value),
            ]));
            assert!(
                matches!(auth, HostAuth::Managed { .. }),
                "{name} makes the switches deliberate, got {auth:?}"
            );
            assert!(
                sanitize(&auth).is_empty(),
                "{name} is an operator's configuration and must survive a spawn"
            );
        }
    }

    /// An empty switch is not a switch, and a falsey one is an operator turning it off.
    ///
    /// *Measured:* `CLAUDE_CODE_HOST_AUTH_ENV_VAR=` and
    /// `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST=0` both leave the keychain working. Reporting
    /// either as a fault is a diagnostic that cries wolf on a correct configuration —
    /// which is how a real warning stops being read.
    #[test]
    fn an_empty_or_falsey_switch_is_not_a_switch() {
        assert_eq!(
            classify(table(&[(HOST_AUTH_POINTER, "")])),
            HostAuth::Untouched
        );
        for off in ["0", "false", "no", "off", "", "2", "xyz"] {
            assert_eq!(
                classify(table(&[(PROVIDER_MANAGED_BY_HOST, off)])),
                HostAuth::Untouched,
                "{off} leaves the keychain working, so it is not a switch"
            );
        }
    }

    /// Every value measured to disable the keychain is treated as on.
    #[test]
    fn every_measured_truthy_spelling_disables_the_keychain() {
        for on in ["1", "true", "TRUE", "True", "yes", "YES", "on", "ON"] {
            assert_eq!(
                classify(table(&[(PROVIDER_MANAGED_BY_HOST, on)])),
                HostAuth::Broken {
                    switches: vec![PROVIDER_MANAGED_BY_HOST.to_string()],
                    endpoint: None,
                },
                "{on} disables the keychain and must be recognized"
            );
        }
    }

    /// **The variable the pointer names is not a credential source.**
    ///
    /// *Measured on 2.1.267:* pointing at `MY_HOST_TOKEN` and setting `MY_HOST_TOKEN`
    /// still reports `loggedIn: false`. So a repair that read the named variable would be
    /// a check that cannot fail — it would call this state healthy and leave the session
    /// refusing to run.
    #[test]
    fn the_variable_the_pointer_names_does_not_count_as_a_credential() {
        let auth = classify(table(&[
            (HOST_AUTH_POINTER, "MY_HOST_TOKEN"),
            ("MY_HOST_TOKEN", "sk-host-token"),
        ]));
        assert_eq!(
            auth,
            HostAuth::Broken {
                switches: vec![HOST_AUTH_POINTER.to_string()],
                endpoint: None,
            },
            "the pointer's own name is inert; only the credentials the CLI reads count"
        );
    }

    /// **A broken switch plus a redirected endpoint strips the endpoint too.**
    ///
    /// The measured incident on this machine, and the case that escapes a switches-only
    /// repair: removing the switches restores the keychain token, which the relay named by
    /// `ANTHROPIC_BASE_URL` then rejects with `401 Invalid token` (`docs/agents.md` § A
    /// host-managed auth switch inherited from another agent session).
    #[test]
    fn a_redirected_endpoint_is_stripped_alongside_a_broken_switch() {
        let auth = classify(table(&[
            (HOST_AUTH_POINTER, "ANTHROPIC_AUTH_TOKEN"),
            (PROVIDER_MANAGED_BY_HOST, "1"),
            (BASE_URL, "https://relay.example/v1"),
        ]));
        assert_eq!(
            auth,
            HostAuth::Broken {
                switches: vec![
                    HOST_AUTH_POINTER.to_string(),
                    PROVIDER_MANAGED_BY_HOST.to_string(),
                ],
                endpoint: Some(BASE_URL.to_string()),
            },
            "a redirected endpoint is part of the broken state, not incidental to it"
        );
        assert_eq!(
            sanitize(&auth),
            vec![
                HOST_AUTH_POINTER.to_string(),
                PROVIDER_MANAGED_BY_HOST.to_string(),
                BASE_URL.to_string(),
            ],
            "all three must leave the spawn, or the session fails with a 401 instead"
        );
    }

    /// **A redirected endpoint with a credential is an operator's relay, left intact.**
    ///
    /// The expensive direction: stripping here reroutes an operator's traffic to Anthropic's
    /// own endpoint, and it would *succeed* there against the wrong credential rather than
    /// fail visibly.
    #[test]
    fn a_managed_relay_keeps_its_endpoint_and_its_credential() {
        for (name, value) in [
            ("ANTHROPIC_AUTH_TOKEN", "sk-host-token"),
            ("ANTHROPIC_API_KEY", "sk-ant-key"),
        ] {
            let auth = classify(table(&[
                (HOST_AUTH_POINTER, "ANTHROPIC_AUTH_TOKEN"),
                (PROVIDER_MANAGED_BY_HOST, "1"),
                (BASE_URL, "https://relay.example/v1"),
                (name, value),
            ]));
            assert_eq!(
                auth,
                HostAuth::Managed {
                    because: format!("{name} carries a credential"),
                },
                "{name} beside a redirected endpoint is a working relay deployment"
            );
            assert!(
                sanitize(&auth).is_empty(),
                "{BASE_URL} must survive: rerouting a relay's traffic is the worse failure"
            );
        }
    }

    /// **A redirected endpoint with no switch at all is not a fault.**
    ///
    /// The CLI's own keychain login works against a relay that accepts it, so classifying a
    /// bare `ANTHROPIC_BASE_URL` as broken would report a fault where there is none — and
    /// would strip it, which is the silent reroute this module must not perform.
    #[test]
    fn a_redirected_endpoint_alone_is_not_a_switch() {
        let auth = classify(table(&[(BASE_URL, "https://relay.example/v1")]));
        assert_eq!(
            auth,
            HostAuth::Untouched,
            "an endpoint without a keychain switch is an operator's own configuration"
        );
        assert!(
            sanitize(&auth).is_empty(),
            "{BASE_URL} is never removed on its own"
        );
    }

    /// **A third-party provider keeps its endpoint, even with no Anthropic credential.**
    ///
    /// The provider answers for the switches, so this is `Managed` and nothing is touched.
    /// Pinned separately from the credential case because it reaches `Managed` through a
    /// different check, and a repair that ordered the endpoint test before the provider test
    /// would strip a working Bedrock or Vertex deployment.
    #[test]
    fn a_third_party_provider_keeps_its_endpoint() {
        for provider in THIRD_PARTY_PROVIDERS {
            let auth = classify(table(&[
                (PROVIDER_MANAGED_BY_HOST, "1"),
                (provider, "1"),
                (BASE_URL, "https://gateway.example/v1"),
            ]));
            assert_eq!(
                auth,
                HostAuth::Managed {
                    because: format!("{provider} selects a third-party provider"),
                },
                "{provider} answers for the switch, so the endpoint is not judged"
            );
            assert!(
                sanitize(&auth).is_empty(),
                "{provider} deployments must keep {BASE_URL}"
            );
        }
    }
}
