//! Where Sylph's own settings and state live.
//!
//! # One root, one file
//!
//! Everything Sylph owns is under `~/.sylph`, and everything a user configures is
//! in `~/.sylph/config.toml`. That is the whole contract, and it replaces six
//! independent environment variables that each carried a `$HOME`-relative default
//! pointing into another project's directory tree.
//!
//! The single root is not tidiness. It is what makes four separate things
//! possible: a directory a new user can be pointed at, a supervisor that has to
//! propagate one variable rather than enumerate every path, a second instance that
//! is one override instead of five, and an uninstall that can describe what it
//! removes. See `docs/configuration.md`.
//!
//! # Precedence
//!
//! Environment, then file, then a default derived from the root. Environment wins
//! so a throwaway instance or a test can override without touching a user's file;
//! the file wins over the default because that is what the file is for. No other
//! location is consulted, and none is inherited from an older layout.
//!
//! # Resolution is pure
//!
//! [`Resolved::from_parts`] takes the environment and the parsed file as
//! arguments rather than reading either. `std::env::set_var` is process-global and
//! `unsafe` in a threaded test binary, so a resolver that read the environment
//! itself could only be tested by one test at a time — which in practice means it
//! is tested for one case and inferred for the rest.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The default root, relative to `$HOME`.
const DEFAULT_HOME_DIR: &str = ".sylph";

/// The identity preamble shipped with the binary, used to seed a new root.
///
/// Compiled in rather than copied from a path at install time: a bundled file
/// beside the binary would be missing from any install that staged the tree
/// differently, and seeding is exactly the moment there is no directory to look
/// in yet.
pub const BUNDLED_SOUL: &str = include_str!("../../resources/SOUL.md");

/// The commented file written on first run.
///
/// Every key is present and commented out, with its default beside it. Nothing is
/// set, which is deliberate: a seeded file that assigned values would be a second
/// copy of the defaults, and the two would drift the first time one changed.
pub const CONFIG_TEMPLATE: &str = include_str!("../../resources/config.toml");

/// The config file exactly as it appears on disk.
///
/// Unknown keys are rejected. A hand-edited file is the one place a typo is both
/// likely and silent — `prot = 4400` would leave the server on 4317 with no
/// diagnostic — and refusing to start naming the key is the only outcome that
/// tells the user what they typed.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(default)]
    pub server: ServerSection,
    #[serde(default)]
    pub paths: PathsSection,
    #[serde(default)]
    pub log: LogSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathsSection {
    pub data_dir: Option<String>,
    pub soul: Option<String>,
    pub quick_start_dir: Option<String>,
    #[serde(default)]
    pub browse_roots: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogSection {
    pub level: Option<String>,
}

impl FileConfig {
    /// Parse a config file, naming the file and the offending key on failure.
    ///
    /// A parse error is fatal. The alternative — warn and continue on defaults —
    /// starts a server that ignores everything the user asked for while reporting
    /// healthy, which is the failure shape this whole module exists to remove.
    pub fn parse(text: &str, path: &Path) -> anyhow::Result<Self> {
        toml::from_str(text).map_err(|error| {
            anyhow::anyhow!("{} is not valid configuration: {error}", path.display())
        })
    }
}

/// Which source supplied a setting, for `doctor` and `GET /config`.
///
/// Reported because "the path is wrong" and "the path I set is not being read"
/// need opposite fixes, and a resolved value alone cannot distinguish them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// An environment variable.
    Env,
    /// The config file.
    File,
    /// Derived from the root, or a built-in constant.
    Default,
}

/// One resolved setting: the value, and where it came from.
#[derive(Debug, Clone)]
pub struct Origin<T> {
    pub value: T,
    pub source: Source,
}

impl<T> Origin<T> {
    fn new(value: T, source: Source) -> Self {
        Self { value, source }
    }
}

/// The environment, as a map, so resolution is a pure function of its inputs.
pub type Env = BTreeMap<String, String>;

/// Read the process environment into the map resolution consumes.
///
/// Only the variables that matter are copied. A full snapshot would put every
/// secret in the process environment into anything that debug-prints this.
pub fn env_snapshot() -> Env {
    const KEYS: &[&str] = &[
        "HOME",
        "SYLPH_HOME",
        "SYLPH_CONFIG",
        "SYLPH_HOST",
        "SYLPH_PORT",
        "SYLPH_DATA_DIR",
        "SYLPH_AGENTS_DIR",
        "SYLPH_SOUL",
        "SYLPH_QUICK_START_DIR",
        "SYLPH_BROWSE_ROOTS",
        "SYLPH_LOG",
        "RUST_LOG",
    ];
    KEYS.iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_string(), value)))
        .collect()
}

/// Expand a leading `~`, and nothing else.
///
/// A hand-edited file is the reason this exists: `~/Workspace/project` is what a person
/// types, and rejecting it would make the file worse to edit than the environment
/// variables it replaces. Only a leading `~` is expanded — `~user`
/// is another user's home and resolving it would need passwd lookups for a case
/// that never arises for a single-user loopback app, so it is left alone rather
/// than half-supported.
pub fn expand_tilde(raw: &str, home: &str) -> PathBuf {
    if raw == "~" {
        return PathBuf::from(home);
    }
    match raw.strip_prefix("~/") {
        Some(rest) => Path::new(home).join(rest),
        None => PathBuf::from(raw),
    }
}

/// Every path and setting, resolved, with the source of each.
///
/// Separate from [`crate::config::Config`], which is what the rest of the server
/// holds: this carries provenance for diagnostics, and `Config` carries only the
/// values so no handler can accidentally branch on where a value came from.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub home: PathBuf,
    /// The user's home directory. Always browsable, and the base for `~`.
    pub user_home: PathBuf,
    pub config_path: PathBuf,
    /// Whether `config_path` existed when resolution ran.
    pub config_present: bool,
    pub host: Origin<String>,
    pub port: Origin<u16>,
    pub data_dir: Origin<PathBuf>,
    /// Where `SOUL.md` was looked for, or `None` when identity was explicitly
    /// declined.
    ///
    /// `None` is not "unset": the adapter protocol reserves an empty `SYLPH_SOUL`
    /// for "inject nothing", so this distinguishes *no identity was asked for* from
    /// *identity resolves to a path that may or may not exist*. Collapsing the two
    /// is what let an install asking for no identity be seeded one anyway.
    pub soul: Origin<Option<PathBuf>>,
    pub quick_start_dir: Origin<PathBuf>,
    pub browse_roots: Origin<Vec<PathBuf>>,
    pub log_level: Origin<String>,
}

/// Where the root is, before anything else can be resolved relative to it.
///
/// Split out because the config file's own location depends on it: `SYLPH_HOME`
/// has to be known before `<home>/config.toml` can be read, so this runs first
/// and its result is passed back in.
pub fn resolve_home(env: &Env) -> (PathBuf, PathBuf) {
    let user_home = env
        .get("HOME")
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        // A process with no `HOME` is exotic, and every path here is relative to
        // it. Falling back to the current directory keeps the server startable —
        // in a container, in a launchd job with a stripped environment — rather
        // than panicking, and `doctor` shows where it landed.
        .unwrap_or_else(|| PathBuf::from("."));

    let home = match env.get("SYLPH_HOME").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(raw) => expand_tilde(raw, &user_home.to_string_lossy()),
        None => user_home.join(DEFAULT_HOME_DIR),
    };

    (home, user_home)
}

/// Where the config file is, given the root.
pub fn resolve_config_path(env: &Env, home: &Path, user_home: &Path) -> PathBuf {
    match env.get("SYLPH_CONFIG").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(raw) => expand_tilde(raw, &user_home.to_string_lossy()),
        None => home.join("config.toml"),
    }
}

impl Resolved {
    /// Resolve every setting from an environment snapshot and a parsed file.
    ///
    /// Pure: nothing here reads the environment, the filesystem, or the clock,
    /// apart from `config_present` which the caller supplies. That is what lets
    /// the whole precedence table be asserted by tests running in parallel.
    pub fn from_parts(env: &Env, file: &FileConfig, config_present: bool) -> Self {
        let (home, user_home) = resolve_home(env);
        let config_path = resolve_config_path(env, &home, &user_home);
        let home_str = user_home.to_string_lossy().into_owned();

        // Three-way resolution, written once per shape rather than per setting.
        // Inlining it eleven times is how one of them ends up reading the file
        // before the environment.
        let string = |env_key: &str, from_file: Option<&String>, default: &str| -> Origin<String> {
            if let Some(value) = env.get(env_key).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                return Origin::new(value.to_string(), Source::Env);
            }
            match from_file.map(|s| s.trim()).filter(|s| !s.is_empty()) {
                Some(value) => Origin::new(value.to_string(), Source::File),
                None => Origin::new(default.to_string(), Source::Default),
            }
        };

        let path = |env_key: &str, from_file: Option<&String>, default: PathBuf| -> Origin<PathBuf> {
            if let Some(value) = env.get(env_key).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                return Origin::new(expand_tilde(value, &home_str), Source::Env);
            }
            match from_file.map(|s| s.trim()).filter(|s| !s.is_empty()) {
                Some(value) => Origin::new(expand_tilde(value, &home_str), Source::File),
                None => Origin::new(default, Source::Default),
            }
        };

        let host = string("SYLPH_HOST", file.server.host.as_ref(), "127.0.0.1");

        // A port that does not parse falls back rather than failing startup: the
        // value is reported by `doctor` and by `GET /config`, and a server that
        // starts on the default port is more useful than one that refuses to run
        // because of a typo in a number. The file's own parse is strict, so this
        // only catches a malformed environment variable.
        let port = match env.get("SYLPH_PORT").and_then(|v| v.trim().parse::<u16>().ok()) {
            Some(value) => Origin::new(value, Source::Env),
            None => match file.server.port {
                Some(value) => Origin::new(value, Source::File),
                None => Origin::new(4317, Source::Default),
            },
        };

        let data_dir = path("SYLPH_DATA_DIR", file.paths.data_dir.as_ref(), home.join("data"));
        // Note the default is the root, not the data directory: `SOUL.md` is
        // hand-edited configuration, and burying it inside the directory whose
        // documented purpose is "machine state you back up and never open" is how
        // a user never finds it.
        //
        // Resolved separately from the paths above because an *explicit empty*
        // value is meaningful here rather than missing: the adapter protocol
        // reserves `SYLPH_SOUL=""` for "inject nothing", and every other setting
        // treats blank as unset. Folding this one into the shared helper is what
        // made an install that asked for no identity get a seeded `SOUL.md` and
        // silent injection.
        let soul = match env.get("SYLPH_SOUL").map(String::as_str) {
            Some(value) if value.trim().is_empty() => Origin::new(None, Source::Env),
            Some(value) => Origin::new(Some(expand_tilde(value.trim(), &home_str)), Source::Env),
            None => match file.paths.soul.as_deref() {
                // The same spelling in the file, for the same reason: an install
                // that never wants identity should be able to say so durably
                // rather than only through its supervisor's environment.
                Some(value) if value.trim().is_empty() => Origin::new(None, Source::File),
                Some(value) => {
                    Origin::new(Some(expand_tilde(value.trim(), &home_str)), Source::File)
                }
                None => Origin::new(Some(home.join("SOUL.md")), Source::Default),
            },
        };
        let quick_start_dir = path(
            "SYLPH_QUICK_START_DIR",
            file.paths.quick_start_dir.as_ref(),
            home.join("scratch"),
        );

        // `$HOME` first and always, so the picker has a landing root even with
        // nothing configured, then whatever was added. Colon-separated in the
        // environment to match `PATH`; a list in the file, because a file that
        // can hold a list should not ask a user to pack one into a string.
        let mut roots = vec![user_home.clone()];
        let roots_source = if let Some(raw) = env
            .get("SYLPH_BROWSE_ROOTS")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            for part in raw.split(':').map(str::trim).filter(|p| !p.is_empty()) {
                let path = expand_tilde(part, &home_str);
                if !roots.contains(&path) {
                    roots.push(path);
                }
            }
            Source::Env
        } else if !file.paths.browse_roots.is_empty() {
            for part in file.paths.browse_roots.iter().map(|s| s.trim()).filter(|p| !p.is_empty()) {
                let path = expand_tilde(part, &home_str);
                if !roots.contains(&path) {
                    roots.push(path);
                }
            }
            Source::File
        } else {
            Source::Default
        };

        // `RUST_LOG` is honoured after `SYLPH_LOG` because it is what the
        // ecosystem's tooling sets and what a developer already has in their
        // shell; dropping it would break every existing debugging habit for no
        // gain.
        let log_level = if env.contains_key("SYLPH_LOG") {
            string("SYLPH_LOG", file.log.level.as_ref(), "info")
        } else if let Some(value) = env.get("RUST_LOG").map(|s| s.trim()).filter(|s| !s.is_empty()) {
            Origin::new(value.to_string(), Source::Env)
        } else {
            string("SYLPH_LOG", file.log.level.as_ref(), "info")
        };

        Self {
            home,
            user_home,
            config_path,
            config_present,
            host,
            port,
            data_dir,
            soul,
            quick_start_dir,
            browse_roots: Origin::new(roots, roots_source),
            log_level,
        }
    }

    /// Resolve from the real environment and filesystem.
    pub fn load() -> anyhow::Result<Self> {
        let env = env_snapshot();
        let (home, user_home) = resolve_home(&env);
        let config_path = resolve_config_path(&env, &home, &user_home);

        let (file, present) = match std::fs::read_to_string(&config_path) {
            Ok(text) => (FileConfig::parse(&text, &config_path)?, true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (FileConfig::default(), false)
            }
            // An unreadable file is not a missing one. Treating a permissions
            // error as absent would silently ignore settings that are right
            // there, which is the failure this module is built to prevent.
            Err(error) => {
                anyhow::bail!("Cannot read {}: {error}", config_path.display())
            }
        };

        Ok(Self::from_parts(&env, &file, present))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn resolve(pairs: &[(&str, &str)], file: FileConfig) -> Resolved {
        Resolved::from_parts(&env(pairs), &file, false)
    }

    #[test]
    fn everything_defaults_under_one_root() {
        let r = resolve(&[("HOME", "/home/u")], FileConfig::default());

        assert_eq!(r.home, PathBuf::from("/home/u/.sylph"));
        assert_eq!(r.config_path, PathBuf::from("/home/u/.sylph/config.toml"));
        assert_eq!(r.data_dir.value, PathBuf::from("/home/u/.sylph/data"));
        assert_eq!(r.soul.value, Some(PathBuf::from("/home/u/.sylph/SOUL.md")));
        assert_eq!(r.quick_start_dir.value, PathBuf::from("/home/u/.sylph/scratch"));
    }

    /// The point of a single root: one variable relocates the whole install, so a
    /// supervisor propagates one thing and a second instance is one override.
    #[test]
    fn one_variable_relocates_every_derived_path() {
        let r = resolve(&[("HOME", "/home/u"), ("SYLPH_HOME", "/opt/inst")], FileConfig::default());

        assert_eq!(r.data_dir.value, PathBuf::from("/opt/inst/data"));
        assert_eq!(r.soul.value, Some(PathBuf::from("/opt/inst/SOUL.md")));
        assert_eq!(r.quick_start_dir.value, PathBuf::from("/opt/inst/scratch"));
        assert_eq!(r.config_path, PathBuf::from("/opt/inst/config.toml"));
    }

    #[test]
    fn the_root_accepts_a_tilde_like_a_hand_edited_path_would() {
        let r = resolve(&[("HOME", "/home/u"), ("SYLPH_HOME", "~/alt")], FileConfig::default());
        assert_eq!(r.home, PathBuf::from("/home/u/alt"));
    }

    /// Environment beats file beats default, which is what makes a throwaway
    /// instance and an overridden test possible without editing a user's file.
    #[test]
    fn the_environment_overrides_the_file_which_overrides_the_default() {
        let mut file = FileConfig::default();
        file.server.port = Some(5000);
        file.paths.data_dir = Some("~/from-file".into());

        let r = Resolved::from_parts(
            &env(&[("HOME", "/home/u"), ("SYLPH_PORT", "6000")]),
            &file,
            true,
        );

        assert_eq!(r.port.value, 6000);
        assert_eq!(r.port.source, Source::Env);
        assert_eq!(r.data_dir.value, PathBuf::from("/home/u/from-file"));
        assert_eq!(r.data_dir.source, Source::File);
        assert_eq!(r.host.value, "127.0.0.1");
        assert_eq!(r.host.source, Source::Default);
    }

    /// An empty variable is not a value. launchd and shell wrappers both produce
    /// `VAR=` for something they meant to leave unset, and honouring it would put
    /// the data directory at the filesystem root.
    #[test]
    fn an_empty_environment_variable_falls_through_rather_than_winning() {
        let mut file = FileConfig::default();
        file.paths.data_dir = Some("/from-file".into());

        let r = Resolved::from_parts(
            &env(&[("HOME", "/home/u"), ("SYLPH_DATA_DIR", "   ")]),
            &file,
            true,
        );

        assert_eq!(r.data_dir.value, PathBuf::from("/from-file"));
        assert_eq!(r.data_dir.source, Source::File);
    }

    /// `SYLPH_SOUL` is the one exception to the rule above, because the adapter
    /// protocol reserves an empty value for "inject nothing".
    ///
    /// This is not a nicety: treating it as unset made an install that asked for no
    /// identity resolve the default path, get a `SOUL.md` seeded there, and inject
    /// it into every session — the exact opposite of what was configured. Caught by
    /// the E2E suite, which sets it to `""` and asserts the UI reports no identity.
    #[test]
    fn an_empty_soul_means_no_identity_rather_than_the_default_path() {
        let r = resolve(&[("HOME", "/home/u"), ("SYLPH_SOUL", "")], FileConfig::default());

        assert_eq!(r.soul.value, None, "empty means decline, not unset");
        assert_eq!(r.soul.source, Source::Env);
    }

    /// The same spelling works in the file, so an install that never wants identity
    /// can say so durably rather than only through its supervisor's environment.
    #[test]
    fn an_empty_soul_in_the_file_also_means_no_identity() {
        let mut file = FileConfig::default();
        file.paths.soul = Some(String::new());

        let r = Resolved::from_parts(&env(&[("HOME", "/home/u")]), &file, true);

        assert_eq!(r.soul.value, None);
        assert_eq!(r.soul.source, Source::File);
    }

    #[test]
    fn a_configured_soul_path_is_expanded_like_any_other() {
        let r = resolve(
            &[("HOME", "/home/u"), ("SYLPH_SOUL", "~/identity/SOUL.md")],
            FileConfig::default(),
        );

        assert_eq!(r.soul.value, Some(PathBuf::from("/home/u/identity/SOUL.md")));
        assert_eq!(r.soul.source, Source::Env);
    }

    /// `$HOME` is always a browse root, so the picker has somewhere to land even
    /// when nothing is configured. Extra roots add to it and never replace it.
    #[test]
    fn browse_roots_always_include_home_and_add_the_configured_ones() {
        let r = resolve(
            &[("HOME", "/home/u"), ("SYLPH_BROWSE_ROOTS", "/vol/a:~/b: :/vol/a")],
            FileConfig::default(),
        );

        assert_eq!(
            r.browse_roots.value,
            vec![
                PathBuf::from("/home/u"),
                PathBuf::from("/vol/a"),
                PathBuf::from("/home/u/b"),
            ],
            "home first, tilde expanded, blanks and duplicates dropped"
        );
        assert_eq!(r.browse_roots.source, Source::Env);
    }

    #[test]
    fn browse_roots_come_from_the_file_as_a_list() {
        let mut file = FileConfig::default();
        file.paths.browse_roots = vec!["/vol/a".into(), "~/b".into()];

        let r = Resolved::from_parts(&env(&[("HOME", "/home/u")]), &file, true);

        assert_eq!(r.browse_roots.value.len(), 3);
        assert_eq!(r.browse_roots.source, Source::File);
    }

    /// `RUST_LOG` keeps working: it is what the ecosystem's tooling sets and what
    /// a developer already has exported, so dropping it would break every existing
    /// debugging habit for nothing.
    #[test]
    fn rust_log_is_honoured_but_sylph_log_wins() {
        let r = resolve(&[("HOME", "/home/u"), ("RUST_LOG", "debug")], FileConfig::default());
        assert_eq!(r.log_level.value, "debug");
        assert_eq!(r.log_level.source, Source::Env);

        let r = resolve(
            &[("HOME", "/home/u"), ("RUST_LOG", "debug"), ("SYLPH_LOG", "trace")],
            FileConfig::default(),
        );
        assert_eq!(r.log_level.value, "trace");
    }

    #[test]
    fn a_file_level_is_used_when_no_variable_is_set() {
        let mut file = FileConfig::default();
        file.log.level = Some("warn".into());

        let r = Resolved::from_parts(&env(&[("HOME", "/home/u")]), &file, true);
        assert_eq!(r.log_level.value, "warn");
        assert_eq!(r.log_level.source, Source::File);
    }

    /// A misspelled key in a hand-edited file must fail loudly. `prot = 4400`
    /// otherwise leaves the server on 4317 with nothing anywhere saying why.
    #[test]
    fn an_unknown_key_is_refused_and_named() {
        let error = FileConfig::parse("[server]\nprot = 4400\n", Path::new("/c/config.toml"))
            .expect_err("unknown keys must not be silently dropped");
        let message = format!("{error}");
        assert!(message.contains("config.toml"), "names the file: {message}");
        assert!(message.contains("prot"), "names the key: {message}");
    }

    #[test]
    fn the_bundled_template_parses_and_sets_nothing() {
        let parsed = FileConfig::parse(CONFIG_TEMPLATE, Path::new("template"))
            .expect("the seeded file must be valid for the parser that will read it");

        // Every key commented out. A template that assigned values would be a
        // second copy of the defaults, free to drift from them.
        assert!(parsed.server.port.is_none());
        assert!(parsed.paths.data_dir.is_none());
        assert!(parsed.log.level.is_none());
    }

    /// The template documents every setting the resolver knows about. A key added
    /// to the struct and not to the file is invisible to every user.
    #[test]
    fn the_template_mentions_every_setting() {
        for key in [
            "host",
            "port",
            "data_dir",
            "soul",
            "quick_start_dir",
            "browse_roots",
            "bin",
            "level",
        ] {
            assert!(
                CONFIG_TEMPLATE.contains(key),
                "the seeded config must document `{key}`"
            );
        }
    }

    #[test]
    fn tilde_expansion_leaves_other_forms_alone() {
        assert_eq!(expand_tilde("~", "/home/u"), PathBuf::from("/home/u"));
        assert_eq!(expand_tilde("~/x", "/home/u"), PathBuf::from("/home/u/x"));
        assert_eq!(expand_tilde("/abs", "/home/u"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("rel/x", "/home/u"), PathBuf::from("rel/x"));
        // `~other` is another user's home; resolving it needs passwd lookups for
        // a case a single-user loopback app never has. Left alone, not guessed.
        assert_eq!(expand_tilde("~other/x", "/home/u"), PathBuf::from("~other/x"));
    }

    /// No `HOME` must not panic: a container or a stripped launchd environment is
    /// where this happens, and a crash there is unattributable.
    #[test]
    fn a_missing_home_falls_back_instead_of_panicking() {
        let r = resolve(&[], FileConfig::default());
        assert_eq!(r.home, PathBuf::from("./.sylph"));
    }
}
