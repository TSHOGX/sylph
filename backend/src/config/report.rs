//! What resolved, where it came from, and whether it is actually there.
//!
//! Every failure in this area has the same shape: a path resolves to somewhere
//! nothing lives, the server starts, `/health` says it is fine, and the symptom
//! surfaces layers away as an empty dropdown or a dead keystroke. The fix is
//! always trivial once you can see which path was used and who supplied it; the
//! cost is entirely in not being able to see it.
//!
//! So this is a first-class surface, not a debug print: `sylph-web doctor` renders
//! it for a terminal, and `GET /api/v1/config` returns it to the settings panel.
//! One producer, so the two cannot disagree.

use std::path::Path;

use serde::Serialize;

use super::resolve::{Origin, Resolved, Source};

/// One resolved setting as reported.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Dotted key as it appears in `config.toml`, e.g. `paths.data_dir`.
    pub key: String,
    pub value: String,
    pub source: Source,
    /// The environment variable that would override this, so a reader never has
    /// to consult a table elsewhere to act on what they are looking at.
    pub env_var: String,
    /// Whether the path exists. `None` for settings that are not paths, which is
    /// distinct from `Some(false)` — "not a path" and "missing" must not render
    /// the same way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
}

/// The whole resolved configuration, plus what discovery found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// The one root everything defaults under.
    pub home: String,
    pub config_path: String,
    /// False on a first run before seeding, and after a user deletes the file.
    pub config_present: bool,
    pub entries: Vec<Entry>,
}

/// Whether a path is a file, a directory, or absent.
fn exists(path: &Path) -> bool {
    path.exists()
}

impl Report {
    /// Build the report from a resolution.
    ///
    /// # What this no longer takes, and why the removal is not a simplification
    ///
    /// It used to be handed the bundled-adapter candidates and the chosen one, and the
    /// reason was sound: recomputing them here would let the report describe a different
    /// directory than the registry searched, which is the class of disagreement this
    /// surface exists to catch. Both arguments go with the directories themselves —
    /// adapters are linked in, so there is nothing to search and nothing to disagree
    /// about. The question that replaces "which directory answered" is "is the vendor CLI
    /// on `PATH`", and it is answered per environment by `GET /environments` rather than
    /// here, because it is a fact about a machine rather than about configuration.
    pub fn build(resolved: &Resolved) -> Self {
        fn text(key: &str, env_var: &str, origin: &Origin<String>) -> Entry {
            Entry {
                key: key.to_string(),
                value: origin.value.clone(),
                source: origin.source,
                env_var: env_var.to_string(),
                exists: None,
            }
        }

        fn path_entry(key: &str, env_var: &str, origin: &Origin<std::path::PathBuf>) -> Entry {
            Entry {
                key: key.to_string(),
                value: origin.value.display().to_string(),
                source: origin.source,
                env_var: env_var.to_string(),
                exists: Some(exists(&origin.value)),
            }
        }

        let entries = vec![
            text("server.host", "SYLPH_HOST", &resolved.host),
            Entry {
                key: "server.port".to_string(),
                value: resolved.port.value.to_string(),
                source: resolved.port.source,
                env_var: "SYLPH_PORT".to_string(),
                exists: None,
            },
            path_entry("paths.data_dir", "SYLPH_DATA_DIR", &resolved.data_dir),
            // Identity has a third state the other paths do not: explicitly
            // declined, which the protocol spells as an empty value. Rendering that
            // as a missing path would report a deliberate choice as a broken
            // setting, so it is named instead and carries no existence flag —
            // there is no path whose existence could be checked.
            Entry {
                key: "paths.soul".to_string(),
                value: match &resolved.soul.value {
                    Some(path) => path.display().to_string(),
                    None => "(none — sessions run on the agent's own identity)".to_string(),
                },
                source: resolved.soul.source,
                env_var: "SYLPH_SOUL".to_string(),
                exists: resolved.soul.value.as_deref().map(exists),
            },
            path_entry(
                "paths.quick_start_dir",
                "SYLPH_QUICK_START_DIR",
                &resolved.quick_start_dir,
            ),
            Entry {
                key: "paths.browse_roots".to_string(),
                value: resolved
                    .browse_roots
                    .value
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":"),
                source: resolved.browse_roots.source,
                env_var: "SYLPH_BROWSE_ROOTS".to_string(),
                exists: None,
            },
            text("log.level", "SYLPH_LOG", &resolved.log_level),
        ];

        Self {
            home: resolved.home.display().to_string(),
            config_path: resolved.config_path.display().to_string(),
            config_present: resolved.config_path.exists(),
            entries,
        }
    }

    /// Render for a terminal.
    ///
    /// Plain text and no colour: this output gets pasted into an issue, and escape
    /// codes there are noise. Aligned so the source column can be read down.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Sylph home    {}\n", self.home));
        out.push_str(&format!(
            "Config file   {}{}\n\n",
            self.config_path,
            if self.config_present {
                ""
            } else {
                "  (absent — defaults in use)"
            }
        ));

        let width = self
            .entries
            .iter()
            .map(|entry| entry.key.len())
            .max()
            .unwrap_or(0);

        for entry in &self.entries {
            let source = match entry.source {
                Source::Env => format!("env {}", entry.env_var),
                Source::File => "config.toml".to_string(),
                Source::Default => "default".to_string(),
            };
            // Only the absent case is annotated. Marking every present path with a
            // tick turns the one line that needs attention into one of nine.
            let missing = match entry.exists {
                Some(false) => "  MISSING",
                _ => "",
            };
            out.push_str(&format!(
                "{:width$}  {}{}\n{:width$}  ({})\n",
                entry.key,
                entry.value,
                missing,
                "",
                source,
                width = width
            ));
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve::FileConfig;

    fn resolved(pairs: &[(&str, &str)]) -> Resolved {
        let env = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Resolved::from_parts(&env, &FileConfig::default(), false)
    }

    fn report(pairs: &[(&str, &str)]) -> Report {
        Report::build(&resolved(pairs))
    }

    /// Every setting is reported, so `doctor` cannot be missing the one that is
    /// wrong. A key added to the resolver and not here is invisible.
    ///
    /// `paths.agents_dir` is deliberately absent, and its absence is asserted from the other
    /// side by `frontend/e2e/settings-config.spec.ts`: adapters are linked in, so there is
    /// no directory to name and a setting that named one would be a lever changing nothing.
    #[test]
    fn every_setting_appears_in_the_report() {
        let report = report(&[("HOME", "/home/u")]);
        let keys: Vec<&str> = report.entries.iter().map(|e| e.key.as_str()).collect();

        for expected in [
            "server.host",
            "server.port",
            "paths.data_dir",
            "paths.soul",
            "paths.quick_start_dir",
            "paths.browse_roots",
            "log.level",
        ] {
            assert!(keys.contains(&expected), "{expected} missing from {keys:?}");
        }
    }

    /// The whole point: a value alone cannot distinguish "the path is wrong" from
    /// "the path I set is being ignored", and those need opposite fixes.
    #[test]
    fn each_entry_names_its_source_and_its_override_variable() {
        let report = report(&[("HOME", "/home/u"), ("SYLPH_PORT", "4400")]);

        let port = report.entries.iter().find(|e| e.key == "server.port").unwrap();
        assert_eq!(port.value, "4400");
        assert_eq!(port.source, Source::Env);
        assert_eq!(port.env_var, "SYLPH_PORT");

        let host = report.entries.iter().find(|e| e.key == "server.host").unwrap();
        assert_eq!(host.source, Source::Default);
        assert_eq!(host.env_var, "SYLPH_HOST");
    }

    /// A missing directory must be visible as missing. This is the fact that turns
    /// "the dropdown is empty" into a one-line diagnosis.
    #[test]
    fn a_path_that_does_not_exist_is_reported_as_missing() {
        let report = report(&[("HOME", "/no/such/home")]);

        let data = report.entries.iter().find(|e| e.key == "paths.data_dir").unwrap();
        assert_eq!(data.exists, Some(false));
        assert!(report.render().contains("MISSING"));
    }

    /// "Not a path" and "missing" must not render the same way, or a log level
    /// would look like a broken directory.
    #[test]
    fn settings_that_are_not_paths_report_no_existence_at_all() {
        let report = report(&[("HOME", "/home/u")]);

        for key in ["server.host", "server.port", "log.level", "paths.browse_roots"] {
            let entry = report.entries.iter().find(|e| e.key == key).unwrap();
            assert_eq!(entry.exists, None, "{key} is not a path");
        }
    }

    /// Identity declined is a supported configuration, not a broken path. Rendering
    /// it as `MISSING` would report a deliberate choice as a fault, and there is no
    /// path whose existence could be checked.
    #[test]
    fn declined_identity_is_named_rather_than_reported_missing() {
        let env = [("HOME", "/home/u"), ("SYLPH_SOUL", "")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let report = Report::build(&Resolved::from_parts(&env, &FileConfig::default(), false));

        let soul = report.entries.iter().find(|e| e.key == "paths.soul").unwrap();
        assert_eq!(soul.exists, None, "there is no path to check");
        assert!(soul.value.contains("none"), "says so plainly: {}", soul.value);
        assert_eq!(soul.source, Source::Env);
    }

    /// Rendered output gets pasted into issues, so it stays plain text.
    #[test]
    fn the_rendered_report_carries_no_escape_codes() {
        let rendered = report(&[("HOME", "/home/u")]).render();
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("/home/u/.sylph"));
        assert!(rendered.contains("config.toml"));
    }

    #[test]
    fn an_absent_config_file_is_stated_rather_than_implied() {
        let rendered = report(&[("HOME", "/no/such/home")]).render();
        assert!(rendered.contains("absent"), "{rendered}");
    }
}
