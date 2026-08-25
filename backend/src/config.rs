use std::env;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// The hot tier: unconsolidated turns. Named for what it holds rather than
    /// when it is read — see [`crate::db`].
    pub journal_path: String,
    pub data_dir: PathBuf,
    pub runners_dir: PathBuf,
    /// Where `<agent>/adapter.sh` lives.
    ///
    /// Separate from `runners_dir` because the two answer different questions:
    /// bundled adapters ship with the server and are versioned with the protocol
    /// they implement, while a user-added agent lives outside the install. Both
    /// are searched, bundled first, so a user can shadow a bundled adapter by
    /// name — which is the escape hatch for a vendor change the bundled one has
    /// not caught up with.
    pub agents_dir: PathBuf,
    pub tracedb_bin: PathBuf,
    /// Directories the browser may enumerate when picking a workspace.
    ///
    /// Defaults to `$HOME` alone. This is the root set for `/api/v1/fs/*`, which
    /// is the only endpoint that lists directories the client names rather than
    /// ones derived from a session — so the bound lives in configuration, not in
    /// the request. Additional roots come from `SYLPH_WEB_BROWSE_ROOTS`, for the
    /// case where projects live outside the home directory (an external volume,
    /// a shared checkout).
    pub browse_roots: Vec<PathBuf>,
    /// Where `SOUL.md` lives, when it exists on disk.
    ///
    /// Resolution mirrors what the runners do, because the two must agree: a
    /// runner falls back to `<its own dir>/../../SOUL.md` when `SYLPH_SOUL` is
    /// unset, which is `<runners_dir>/../SOUL.md` from here. Resolving it in
    /// config rather than at each call site is what lets the settings panel show
    /// the *same* file the runner will read, instead of a second guess at where
    /// it might be.
    ///
    /// `None` means no readable `SOUL.md` was found. That is not an error: an
    /// install without one runs every session natively, and the UI says so
    /// rather than offering a toggle over a file that does not exist.
    pub soul_path: Option<PathBuf>,
    pub cookie_secret: String,
    /// Where consolidated history lives. Derived from the data directory rather
    /// than configured separately: the two tiers are one store split for SQLite's
    /// sake, not two things a user chooses between.
    pub history_path: String,
    /// Root of the content-addressed blob store.
    pub blobs_dir: PathBuf,
    /// Every directory `agents_dir` resolution considered, in order.
    ///
    /// Kept so "zero agents discovered" can name where it looked. Without it the
    /// operator is told only that nothing was found, which is the one fact that
    /// does not help — a missing install and a mistyped `SYLPH_AGENTS_DIR` produce
    /// the identical message and need opposite fixes.
    pub agents_dir_candidates: Vec<PathBuf>,
}

/// Where bundled `adapter.sh` trees live, and everywhere that was tried.
///
/// A bundled adapter is versioned with the protocol it implements, so it ships
/// *with the binary* rather than being installed separately. That makes the
/// binary's own location the authoritative answer, and it is the only candidate
/// that stays correct when the tree is moved or copied to another machine.
///
/// The compile-time repo path is last on purpose. It is a path on whichever
/// machine ran `cargo build`, so it is right for `cargo run` and meaningless
/// anywhere else — a release binary that fell back to it would claim to have
/// found adapters in a directory that does not exist on the host. It is a dev
/// convenience, never a deployment answer.
///
/// Every candidate is returned, not just the winner, because "zero agents
/// discovered" is otherwise unanswerable: the operator needs to know which
/// directories were consulted before they can tell a missing install from a
/// misconfigured `SYLPH_AGENTS_DIR`.
fn resolve_agents_dir(
    home: &str,
    configured: Option<&str>,
    exe_dir: Option<&Path>,
) -> (PathBuf, Vec<PathBuf>) {
    // An explicit setting is obeyed even when it does not exist. Silently
    // searching past it would hide the operator's own typo behind a directory
    // they did not name, and report success for adapters they did not choose.
    if let Some(path) = configured.filter(|p| !p.trim().is_empty()) {
        let path = PathBuf::from(path.trim());
        return (path.clone(), vec![path]);
    }

    let mut candidates = Vec::new();
    if let Some(dir) = exe_dir {
        // An install stages adapters beside the binary.
        candidates.push(dir.join("adapters"));
        // `target/debug/<bin>` in a dev checkout: three levels up is the repo.
        if let Some(repo) = dir.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            candidates.push(repo.join("adapters"));
        }
    }
    candidates.push(PathBuf::from(format!("{home}/.local/share/sylph-web/adapters")));
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("backend crate has a parent")
            .join("adapters"),
    );

    let chosen = candidates
        .iter()
        .find(|path| path.is_dir())
        .cloned()
        // Nothing exists: report the first candidate so the error names the place
        // an install is expected to put them, rather than the last fallback.
        .unwrap_or_else(|| candidates[0].clone());

    (chosen, candidates)
}

/// Resolve `SOUL.md` the way a runner resolves it.
///
/// An explicit empty `SYLPH_SOUL` is meaningful, not missing: the runner protocol
/// reserves it for "no identity", and the E2E fixtures rely on it to keep runs
/// cheap. It therefore yields `None` here too, so the backend and the runner
/// reach the same conclusion.
///
/// A path that does not exist also yields `None`, matching the runner's
/// `[[ -f "$SOUL_PATH" ]] || SOUL_PATH=""` line. Reporting a configured-but-absent
/// path as present would let the UI promise an injection that never happens.
///
/// The env read is a parameter so this is a pure function: `std::env::set_var` is
/// process-global and unsafe in a threaded test binary, so a test that had to set
/// it could not run beside the others.
fn resolve_soul_path(runners_dir: &Path, configured: Option<&str>) -> Option<PathBuf> {
    let candidate = match configured {
        Some("") => return None,
        Some(value) => PathBuf::from(value),
        None => runners_dir.parent()?.join("SOUL.md"),
    };
    candidate.is_file().then_some(candidate)
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let home = env::var("HOME").expect("HOME not set");

        let host = env::var("SYLPH_WEB_HOST")
            .unwrap_or_else(|_| "127.0.0.1".to_string());

        let port = env::var("SYLPH_WEB_PORT")
            .unwrap_or_else(|_| "4317".to_string())
            .parse::<u16>()?;

        let data_dir = env::var("SYLPH_WEB_DATA_DIR")
            .unwrap_or_else(|_| format!("{}/.agents/data/sylph/web", home))
            .into();

        let runners_dir = PathBuf::from(
            env::var("SYLPH_RUNNERS_DIR")
                .unwrap_or_else(|_| format!("{}/.agents/skills/sylph/runners", home)),
        );

        // The binary's own directory, which is where an install stages its
        // bundled adapters. `current_exe` can fail on an exotic platform, so the
        // resolver takes an `Option` and falls back to the home and repo paths
        // rather than this being a hard startup failure.
        let exe_dir = env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let (agents_dir, agents_dir_candidates) = resolve_agents_dir(
            &home,
            env::var("SYLPH_AGENTS_DIR").ok().as_deref(),
            exe_dir.as_deref(),
        );

        let tracedb_bin = env::var("TRACEDB_BIN")
            .unwrap_or_else(|_| format!("{}/Workspace/trace-db/target/release/trace-db", home))
            .into();

        let journal_path = format!("{}/journal.db", PathBuf::from(&data_dir).display());
        let history_path = format!("{}/history.db", PathBuf::from(&data_dir).display());
        let blobs_dir = PathBuf::from(&data_dir).join("cas");

        // `$HOME` first so it is the default landing root in the picker, then any
        // configured extras. Existence is not checked here: a root that vanishes
        // later must be handled at request time anyway, so the endpoint filters
        // rather than the constructor failing at startup over a stale env var.
        let mut browse_roots = vec![PathBuf::from(&home)];
        if let Ok(extra) = env::var("SYLPH_WEB_BROWSE_ROOTS") {
            for part in extra.split(':').filter(|p| !p.trim().is_empty()) {
                let path = PathBuf::from(part.trim());
                if !browse_roots.contains(&path) {
                    browse_roots.push(path);
                }
            }
        }

        let configured_soul = env::var("SYLPH_SOUL").ok();
        let soul_path = resolve_soul_path(&runners_dir, configured_soul.as_deref());

        // Generate random cookie secret on startup
        let cookie_secret = uuid::Uuid::new_v4().to_string();

        Ok(Self {
            host,
            port,
            journal_path,
            data_dir,
            runners_dir,
            agents_dir,
            tracedb_bin,
            browse_roots,
            soul_path,
            cookie_secret,
            history_path,
            blobs_dir,
            agents_dir_candidates,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The default location has to match the runners' own fallback
    /// (`<runners_dir>/../SOUL.md`), or the settings panel shows a different
    /// file than the one that gets injected.
    #[test]
    fn the_default_soul_path_is_the_parent_of_the_runners_directory() {
        let tmp = TempDir::new().unwrap();
        let runners = tmp.path().join("runners");
        std::fs::create_dir_all(&runners).unwrap();
        let soul = tmp.path().join("SOUL.md");
        std::fs::write(&soul, "# SOUL.md").unwrap();

        assert_eq!(resolve_soul_path(&runners, None), Some(soul));
    }

    #[test]
    fn an_absent_soul_file_resolves_to_none_rather_than_a_path_that_cannot_be_read() {
        let tmp = TempDir::new().unwrap();
        let runners = tmp.path().join("runners");
        std::fs::create_dir_all(&runners).unwrap();

        assert_eq!(resolve_soul_path(&runners, None), None);
    }

    #[test]
    fn an_explicit_path_overrides_the_default_location() {
        let tmp = TempDir::new().unwrap();
        let runners = tmp.path().join("runners");
        std::fs::create_dir_all(&runners).unwrap();
        // A default alongside, to prove the override wins rather than merges.
        std::fs::write(tmp.path().join("SOUL.md"), "default").unwrap();
        let explicit = tmp.path().join("elsewhere.md");
        std::fs::write(&explicit, "explicit").unwrap();

        let resolved = resolve_soul_path(&runners, Some(explicit.to_str().unwrap()));
        assert_eq!(resolved, Some(explicit));
    }

    /// The runner protocol reserves the empty string for "no identity". The
    /// backend has to read it the same way, or it would report a SOUL.md as
    /// active while every runner it spawns is told to skip it.
    #[test]
    fn an_empty_configured_path_means_no_soul_not_the_default_one() {
        let tmp = TempDir::new().unwrap();
        let runners = tmp.path().join("runners");
        std::fs::create_dir_all(&runners).unwrap();
        std::fs::write(tmp.path().join("SOUL.md"), "# SOUL.md").unwrap();

        assert_eq!(resolve_soul_path(&runners, Some("")), None);
    }

    #[test]
    fn agents_dir_prefers_an_explicit_override() {
        let tmp = TempDir::new().unwrap();
        let explicit = tmp.path().join("mine");
        std::fs::create_dir_all(&explicit).unwrap();

        let (chosen, candidates) =
            resolve_agents_dir("/unused", Some(explicit.to_str().unwrap()), None);
        assert_eq!(chosen, explicit);
        assert_eq!(candidates, vec![explicit]);
    }

    /// An explicit setting that does not exist must not be searched past.
    ///
    /// Falling through to a directory the operator did not name would report
    /// adapters they did not choose, and hide their own typo while doing it.
    #[test]
    fn an_explicit_agents_dir_is_obeyed_even_when_absent() {
        let (chosen, candidates) = resolve_agents_dir("/unused", Some("/no/such/adapters"), None);

        assert_eq!(chosen, PathBuf::from("/no/such/adapters"));
        assert_eq!(candidates.len(), 1);
    }

    /// An install stages adapters beside the binary, which is the only candidate
    /// that survives the tree being moved or copied to another machine.
    #[test]
    fn agents_dir_prefers_the_directory_beside_the_binary() {
        let tmp = TempDir::new().unwrap();
        let beside = tmp.path().join("adapters");
        std::fs::create_dir_all(&beside).unwrap();

        let (chosen, _) = resolve_agents_dir("/no/such/home", None, Some(tmp.path()));
        assert_eq!(chosen, beside);
    }

    /// `target/debug/<bin>` in a dev checkout: the repo is three levels up, and
    /// finding it there is what makes `cargo run` work with no env var set.
    #[test]
    fn agents_dir_finds_the_repo_from_a_target_debug_binary() {
        let tmp = TempDir::new().unwrap();
        let exe_dir = tmp.path().join("backend/target/debug");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let repo_adapters = tmp.path().join("adapters");
        std::fs::create_dir_all(&repo_adapters).unwrap();

        let (chosen, _) = resolve_agents_dir("/no/such/home", None, Some(&exe_dir));
        assert_eq!(chosen, repo_adapters);
    }

    #[test]
    fn agents_dir_falls_back_to_the_repo_adapters_tree_in_a_dev_checkout() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("adapters");
        assert!(repo.is_dir(), "repo adapters/ should exist in this checkout");

        let (chosen, _) = resolve_agents_dir("/no/such/home", None, None);
        assert_eq!(chosen, repo);
    }

    /// The install location is offered before the home and compile-time paths, and
    /// every place tried is reported.
    ///
    /// This cannot assert the all-absent case: the last candidate is this repo's
    /// own `adapters/`, which exists whenever the tests run. So it pins the
    /// ordering — the directory beside the binary is asked about first — which is
    /// the property that makes a moved or copied install still find its adapters.
    #[test]
    fn agents_dir_reports_every_place_it_looked_install_first() {
        let tmp = TempDir::new().unwrap();
        let exe_dir = tmp.path().join("prefix");
        std::fs::create_dir_all(&exe_dir).unwrap();

        let (_, candidates) = resolve_agents_dir("/no/such/home", None, Some(&exe_dir));

        assert_eq!(candidates[0], exe_dir.join("adapters"));
        assert!(
            candidates.contains(&PathBuf::from("/no/such/home/.local/share/sylph-web/adapters")),
            "the install path is still consulted: {candidates:?}"
        );
    }

    /// A configured path that points at a directory is not a readable soul.
    #[test]
    fn a_directory_is_not_accepted_as_a_soul_file() {
        let tmp = TempDir::new().unwrap();
        let runners = tmp.path().join("runners");
        std::fs::create_dir_all(&runners).unwrap();

        assert_eq!(resolve_soul_path(&runners, Some(tmp.path().to_str().unwrap())), None);
    }
}
