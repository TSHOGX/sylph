//! Configuration the rest of the server holds.
//!
//! [`resolve`] answers *where things are and who said so*; this module turns that
//! into the flat value object every handler reads, and owns the side effects that
//! come with it — creating the root on first run, seeding a documented
//! `config.toml` and a `SOUL.md`, and reporting what resolved.
//!
//! Provenance is carried alongside rather than mixed in. [`Config`]'s own fields
//! are flat values, so a handler reads a path without being able to accidentally
//! branch on whether it came from the environment or the file; the [`Resolved`] it
//! was built from is kept behind [`Config::provenance`] for the two surfaces that
//! legitimately need it — `doctor` and `GET /api/v1/config`.

pub mod report;
pub mod resolve;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use report::Report;
pub use resolve::{Resolved, Source};

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,

    /// The root everything else defaults under. Reported, not read: every path
    /// below is already resolved.
    pub home: PathBuf,
    /// The config file's path, so the UI can tell a user what to open.
    pub config_path: PathBuf,

    pub data_dir: PathBuf,
    /// The hot tier: unconsolidated turns. Named for what it holds rather than
    /// when it is read — see [`crate::db`].
    pub journal_path: String,
    /// Consolidated history. Derived from the data directory rather than
    /// configured: the two tiers are one store split for SQLite's sake, not two
    /// things a user chooses between.
    pub history_path: String,
    /// Root of the content-addressed blob store.
    pub blobs_dir: PathBuf,

    /// Where the user's own `<agent>/adapter.sh` trees live.
    ///
    /// Distinct from [`Self::bundled_agents_dir`] because the two answer different
    /// questions. A bundled adapter is versioned with the protocol it implements
    /// and is replaced on upgrade; this directory is the user's, survives upgrades,
    /// and shadows a bundled adapter of the same name — the escape hatch for a
    /// vendor change the shipped adapter has not caught up with.
    pub agents_dir: PathBuf,
    /// Where adapters that ship with the binary live.
    pub bundled_agents_dir: PathBuf,
    /// Every directory bundled-adapter resolution considered, in order.
    ///
    /// Kept so "zero agents discovered" can name where it looked. Without it a
    /// missing install and a mistyped path produce the identical message and need
    /// opposite fixes.
    pub bundled_agents_candidates: Vec<PathBuf>,

    /// The identity preamble, when a readable one exists.
    ///
    /// `None` means either that no `SOUL.md` was found or that identity was
    /// explicitly declined. Neither is an error: such an install runs every session
    /// on the agent's own identity, and the UI says so rather than offering a toggle
    /// over a file that does not exist. Resolved once, here, so the settings panel
    /// shows the same file the adapter will read.
    pub soul_path: Option<PathBuf>,
    /// Where `SOUL.md` was looked for, or `None` when identity was declined
    /// outright. Needed to tell a user where to put one — and to know when there is
    /// nowhere, because they asked for none.
    pub soul_candidate: Option<PathBuf>,

    /// Where a Quick Start session runs.
    pub quick_start_dir: PathBuf,

    /// Directories the browser may enumerate when picking a workspace.
    ///
    /// The root set for `/api/v1/fs/*`, the only endpoint that lists directories
    /// the client names rather than ones derived from a session — so the bound
    /// lives in configuration, not in the request.
    pub browse_roots: Vec<PathBuf>,

    pub cookie_secret: String,

    /// What resolved and who supplied it.
    ///
    /// Behind an `Arc` because `Config` is cloned into every handler and this is
    /// read by exactly one of them. Kept at all because the alternative — building
    /// the report at startup and storing that — would freeze its `exists` checks,
    /// so a user who created a missing directory would still be told it was
    /// missing.
    pub provenance: Arc<Resolved>,
}

impl Config {
    /// Resolve, seed a first run, and build.
    ///
    /// Seeding happens here rather than in an installer because `cargo run` is a
    /// first run too: an app whose directory only exists after a shell script has
    /// been run is an app that behaves differently for developers than for users,
    /// and the developer path is the one that gets tested.
    pub fn load() -> anyhow::Result<Self> {
        let resolved = Resolved::load()?;
        let seeded = seed(&resolved)?;
        for note in &seeded {
            tracing::info!("{note}");
        }
        Ok(Self::from_resolved(&resolved))
    }

    /// Build from an already-resolved set of paths, with no filesystem writes.
    pub fn from_resolved(resolved: &Resolved) -> Self {
        let data_dir = resolved.data_dir.value.clone();
        let (bundled_agents_dir, bundled_agents_candidates) = bundled_agents_dir();

        Self {
            host: resolved.host.value.clone(),
            port: resolved.port.value,
            home: resolved.home.clone(),
            config_path: resolved.config_path.clone(),
            journal_path: data_dir.join("journal.db").display().to_string(),
            history_path: data_dir.join("history.db").display().to_string(),
            blobs_dir: data_dir.join("blobs"),
            data_dir,
            agents_dir: resolved.agents_dir.value.clone(),
            bundled_agents_dir,
            bundled_agents_candidates,
            soul_path: resolved.soul.value.as_deref().and_then(readable_soul),
            soul_candidate: resolved.soul.value.clone(),
            quick_start_dir: resolved.quick_start_dir.value.clone(),
            browse_roots: resolved.browse_roots.value.clone(),
            // Regenerated per process, which invalidates open browser sessions on
            // restart by design. Persisting it would turn a token that lives in
            // memory into one that lives on disk, for the sake of not reloading
            // a page.
            cookie_secret: uuid::Uuid::new_v4().to_string(),
            provenance: Arc::new(resolved.clone()),
        }
    }

    /// The resolution report, rebuilt against the live filesystem.
    ///
    /// Rebuilt per call rather than cached: every `exists` field in it is a
    /// filesystem fact, and a cached report would keep telling a user their data
    /// directory is missing after they created it.
    pub fn report(&self) -> Report {
        Report::build(
            &self.provenance,
            &self.bundled_agents_candidates,
            &self.bundled_agents_dir,
        )
    }

    /// Directories agent discovery searches, bundled first.
    ///
    /// Bundled first so a user-installed adapter of the same name *shadows* rather
    /// than duplicates — first root wins in the registry.
    pub fn agent_roots(&self) -> Vec<PathBuf> {
        vec![self.bundled_agents_dir.clone(), self.agents_dir.clone()]
    }

    /// Where a client may name a session's working directory.
    ///
    /// # Why this is `browse_roots` and not a key of its own
    ///
    /// A workdir is where an agent process is *started*, which is a strictly
    /// larger capability than listing directory names — so the first instinct is
    /// that it deserves its own, narrower bound. It does not, and the reason is
    /// that `/api/v1/fs/*` has exactly one purpose: naming a workdir. Nothing
    /// else consumes it, and its response documents as much ("the client hands
    /// this straight back as a session `workdir`"). A second key would therefore
    /// have to be kept equal to this one to keep the picker honest, and two keys
    /// that must agree are two keys that will not: widening one alone yields
    /// either a picker offering directories creation refuses, or — far worse —
    /// creation accepting directories the operator only meant to be listable.
    /// One key, one blast radius, stated once in `docs/configuration.md`.
    ///
    /// The Quick Start directory is added because it is where the product's own
    /// no-workspace sessions run, and it is only *coincidentally* browsable: it
    /// defaults under `<home>/scratch`, and both `SYLPH_HOME` and
    /// `paths.quick_start_dir` can move it outside `$HOME`. An install that did
    /// so would find Quick Start refused by a bound the user never aimed at it.
    ///
    /// Canonicalized here, and roots that no longer exist are dropped, so a
    /// stale entry cannot make the bound accidentally wider *or* refuse
    /// everything: a non-canonical root rejects every path beneath it on macOS,
    /// where `/tmp` is a symlink to `/private/tmp`.
    pub fn session_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::with_capacity(self.browse_roots.len() + 1);
        for candidate in self
            .browse_roots
            .iter()
            .chain(std::iter::once(&self.quick_start_dir))
        {
            let Ok(canonical) = std::fs::canonicalize(candidate) else {
                continue;
            };
            if canonical.is_dir() && !roots.contains(&canonical) {
                roots.push(canonical);
            }
        }
        roots
    }
}

/// A configured `SOUL.md` that can actually be read.
///
/// A path that does not resolve to a file yields `None`, matching what the
/// adapters conclude. Reporting a configured-but-absent file as present would let
/// the UI promise an injection that never happens.
fn readable_soul(candidate: &Path) -> Option<PathBuf> {
    candidate.is_file().then(|| candidate.to_path_buf())
}
/// Where adapters shipped with the binary live, and everywhere that was tried.
///
/// The binary's own directory is authoritative, and is the only candidate that
/// stays correct when an install tree is moved or copied to another machine. The
/// compile-time repo path is last on purpose: it is a path on whichever machine
/// ran `cargo build`, right for `cargo run` and meaningless anywhere else.
///
/// `SYLPH_BUNDLED_AGENTS_DIR` overrides the search, and is deliberately
/// environment-only rather than a `config.toml` key. It exists for a test suite
/// and a packager — E2E has to pin this root or bundled resolution finds the
/// repo's real adapters and the suite runs the developer's actual agent CLIs — and
/// it is not a choice a user makes: which adapters ship with a binary is decided
/// by whoever built it. Offering it in the file would invite pointing a supported
/// setting at adapters versioned against another protocol release.
fn bundled_agents_dir() -> (PathBuf, Vec<PathBuf>) {
    if let Some(pinned) = std::env::var("SYLPH_BUNDLED_AGENTS_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(pinned);
        return (path.clone(), vec![path]);
    }

    let mut candidates = Vec::new();

    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        // An install stages adapters beside the binary.
        candidates.push(dir.join("adapters"));
        // `backend/target/{debug,release}/<bin>` in a dev checkout: three levels
        // up is the repo.
        if let Some(repo) = dir.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            candidates.push(repo.join("adapters"));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the backend crate has a parent directory")
            .join("adapters"),
    );

    // In a dev checkout the exe-relative walk and the compile-time path are the
    // *same* directory, which listed it twice and — since the report marks the
    // chosen path by equality — showed two rows both claiming to be in use. A
    // reader cannot act on a list that contradicts itself about which directory is
    // being read.
    //
    // Order-preserving, and not `Vec::dedup`, which only collapses *adjacent*
    // duplicates: the two spellings that coincide are not always neighbours.
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|path| seen.insert(path.clone()));
    let chosen = candidates
        .iter()
        .find(|path| path.is_dir())
        .cloned()
        // Nothing exists: name the first candidate, so the error points at where
        // an install is expected to put them rather than at the last fallback.
        .unwrap_or_else(|| candidates[0].clone());

    (chosen, candidates)
}

/// Create the root and seed what a first run needs, returning what was written.
///
/// Idempotent, and never overwrites.
///
/// "First run" means *the config file did not exist*, not merely that some file is
/// missing now. The difference matters for `SOUL.md`: deleting it is a meaningful
/// act — the protocol reserves the empty case for "no identity" — so a seeding rule
/// keyed only on its absence would restore it on the next restart and quietly
/// override a deliberate choice. Use [`seed_soul`] to put it back on request.
pub fn seed(resolved: &Resolved) -> anyhow::Result<Vec<String>> {
    // Read before anything is written, since writing the config file is what makes
    // this false for every later call.
    let first_run = !resolved.config_path.exists();
    let mut notes = Vec::new();

    // Only the directories this install owns. A `data_dir` or `agents_dir` pointed
    // outside the root is still created, because a configured path that the server
    // then fails to write to is a worse outcome than a created directory.
    for dir in [
        &resolved.home,
        &resolved.data_dir.value,
        &resolved.agents_dir.value,
        &resolved.quick_start_dir.value,
    ] {
        std::fs::create_dir_all(dir)
            .map_err(|e| anyhow::anyhow!("Cannot create {}: {e}", dir.display()))?;
    }

    if first_run {
        if let Some(parent) = resolved.config_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&resolved.config_path, resolve::CONFIG_TEMPLATE).map_err(|e| {
            anyhow::anyhow!("Cannot write {}: {e}", resolved.config_path.display())
        })?;
        notes.push(format!(
            "Wrote a commented configuration file at {}",
            resolved.config_path.display()
        ));

        notes.extend(seed_soul(resolved)?);
    }

    Ok(notes)
}

/// Write the bundled `SOUL.md` to the resolved path, if nothing is there.
///
/// Separate from [`seed`] so that restoring identity is something a user asks for
/// (`config init`) rather than something a restart does to them.
///
/// Nothing is written when identity was declined outright — an empty `SYLPH_SOUL`
/// or `paths.soul = ""` — because seeding a file there would silently re-enable the
/// injection the install asked not to have. Nor when the parent directory is
/// missing: a `paths.soul` pointing somewhere absent is a configuration error to
/// report, not one to paper over by creating directories the user did not ask for.
pub fn seed_soul(resolved: &Resolved) -> anyhow::Result<Vec<String>> {
    let Some(soul) = resolved.soul.value.as_ref() else {
        return Ok(Vec::new());
    };
    if soul.exists() || !soul.parent().map(Path::is_dir).unwrap_or(false) {
        return Ok(Vec::new());
    }
    std::fs::write(soul, resolve::BUNDLED_SOUL)
        .map_err(|e| anyhow::anyhow!("Cannot write {}: {e}", soul.display()))?;
    Ok(vec![format!(
        "Seeded the identity preamble at {}",
        soul.display()
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn resolved_in(root: &Path) -> Resolved {
        let env = [("HOME", root.display().to_string())]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        Resolved::from_parts(&env, &resolve::FileConfig::default(), false)
    }

    /// The resolved `SOUL.md` path, for the tests that write or delete one.
    ///
    /// Panics when identity was declined, which is the correct outcome: a test
    /// reaching for the path in that case has confused "no file" with "no identity
    /// asked for", and those are the two states this module keeps apart.
    fn soul_of(resolved: &Resolved) -> &Path {
        resolved
            .soul
            .value
            .as_deref()
            .expect("this resolution asked for identity")
    }

    /// A first run must leave a usable, documented install behind — including for
    /// `cargo run`, which is the path that actually gets exercised.
    #[test]
    fn a_first_run_creates_the_root_and_seeds_it() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolved_in(tmp.path());

        let notes = seed(&resolved).unwrap();

        assert!(resolved.home.is_dir());
        assert!(resolved.data_dir.value.is_dir());
        assert!(resolved.agents_dir.value.is_dir());
        assert!(resolved.quick_start_dir.value.is_dir());
        assert!(resolved.config_path.is_file());
        assert!(soul_of(&resolved).is_file());
        assert_eq!(notes.len(), 2, "the config and the soul are both reported");

        // The seeded file must parse with the parser that will read it next start.
        let text = std::fs::read_to_string(&resolved.config_path).unwrap();
        resolve::FileConfig::parse(&text, &resolved.config_path).unwrap();
    }

    /// Seeding on every start would silently revert a user's edits.
    #[test]
    fn seeding_never_overwrites_what_is_already_there() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolved_in(tmp.path());
        seed(&resolved).unwrap();

        std::fs::write(&resolved.config_path, "[server]\nport = 9999\n").unwrap();
        std::fs::write(soul_of(&resolved), "# mine").unwrap();

        let notes = seed(&resolved).unwrap();

        assert!(notes.is_empty(), "a second run writes nothing: {notes:?}");
        assert_eq!(
            std::fs::read_to_string(&resolved.config_path).unwrap(),
            "[server]\nport = 9999\n"
        );
        assert_eq!(std::fs::read_to_string(soul_of(&resolved)).unwrap(), "# mine");
    }

    /// Deleting `SOUL.md` means "no identity", which the protocol reserves. An app
    /// that restored it would be overriding a deliberate choice on every restart.
    #[test]
    fn a_deleted_soul_stays_deleted() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolved_in(tmp.path());
        seed(&resolved).unwrap();
        std::fs::remove_file(soul_of(&resolved)).unwrap();

        // The config file now exists, so this is the ordinary restart path.
        let notes = seed(&resolved).unwrap();

        assert!(
            !soul_of(&resolved).exists(),
            "a restart must not re-create it: {notes:?}"
        );
        assert!(Config::from_resolved(&resolved).soul_path.is_none());
    }

    /// Restoring it is available, just not automatic: `config init` asks for it
    /// explicitly, which is the difference between offering and overriding.
    #[test]
    fn a_deleted_soul_can_be_restored_on_request() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolved_in(tmp.path());
        seed(&resolved).unwrap();
        std::fs::remove_file(soul_of(&resolved)).unwrap();

        let notes = seed_soul(&resolved).unwrap();

        assert_eq!(notes.len(), 1);
        assert!(soul_of(&resolved).is_file());
        assert_eq!(
            std::fs::read_to_string(soul_of(&resolved)).unwrap(),
            resolve::BUNDLED_SOUL
        );
    }

    /// Restoring must not overwrite an edited file either, so the operation is
    /// safe to run on an install that is already set up.
    #[test]
    fn restoring_leaves_an_existing_soul_alone() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolved_in(tmp.path());
        seed(&resolved).unwrap();
        std::fs::write(soul_of(&resolved), "# mine").unwrap();

        assert!(seed_soul(&resolved).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(soul_of(&resolved)).unwrap(), "# mine");
    }

    #[test]
    fn an_absent_soul_resolves_to_none_but_still_reports_where_it_looked() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolved_in(tmp.path());
        let config = Config::from_resolved(&resolved);

        assert!(config.soul_path.is_none());
        assert_eq!(config.soul_candidate, resolved.soul.value);
    }

    /// Declining identity must not be answered with a seeded file.
    ///
    /// The protocol reserves an empty `SYLPH_SOUL` for "inject nothing", so seeding
    /// the default path would turn an explicit refusal into silent injection — which
    /// is exactly what happened until the resolver stopped treating empty as unset.
    #[test]
    fn declining_identity_seeds_nothing_and_reports_no_candidate() {
        let tmp = TempDir::new().unwrap();
        let env = [
            ("HOME", tmp.path().display().to_string()),
            ("SYLPH_SOUL", String::new()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let resolved = Resolved::from_parts(&env, &resolve::FileConfig::default(), false);

        let notes = seed(&resolved).unwrap();

        assert!(
            !tmp.path().join(".sylph").join("SOUL.md").exists(),
            "no identity file may appear: {notes:?}"
        );
        // And `config init` must not undo the decision either.
        assert!(seed_soul(&resolved).unwrap().is_empty());

        let config = Config::from_resolved(&resolved);
        assert!(config.soul_path.is_none());
        assert!(
            config.soul_candidate.is_none(),
            "there is no candidate, because none was asked for"
        );
    }

    /// The two tiers and the blob store are one store split for SQLite's sake, so
    /// they must land together under whatever data directory was configured.
    #[test]
    fn both_database_tiers_and_the_blob_store_sit_under_the_data_directory() {
        let tmp = TempDir::new().unwrap();
        let config = Config::from_resolved(&resolved_in(tmp.path()));

        let data = config.data_dir.display().to_string();
        assert!(config.journal_path.starts_with(&data));
        assert!(config.history_path.starts_with(&data));
        assert!(config.blobs_dir.starts_with(&config.data_dir));
    }

    /// Bundled first, so a user adapter of the same name shadows rather than
    /// duplicates — the registry takes the first root that answers.
    #[test]
    fn agent_roots_search_bundled_adapters_before_the_users_own() {
        let tmp = TempDir::new().unwrap();
        let config = Config::from_resolved(&resolved_in(tmp.path()));

        let roots = config.agent_roots();
        assert_eq!(roots[0], config.bundled_agents_dir);
        assert_eq!(roots[1], config.agents_dir);
        assert!(
            config.agents_dir.starts_with(&config.home),
            "the user's agents live inside the one root"
        );
    }

    /// A dev checkout must find the repo's `adapters/`, which is what makes
    /// `cargo run` work with nothing configured.
    #[test]
    fn bundled_adapters_are_found_in_a_dev_checkout() {
        let (chosen, candidates) = bundled_agents_dir();
        assert!(chosen.is_dir(), "chose {chosen:?} out of {candidates:?}");
        assert!(chosen.ends_with("adapters"));
    }

    /// One directory, one row.
    ///
    /// In a dev checkout the exe-relative walk and the compile-time path resolve to
    /// the same place, so the list held it twice — and because the report marks the
    /// chosen entry by equality, both rows claimed to be the one in use. A
    /// diagnostic that contradicts itself about which directory is read is worse
    /// than none.
    #[test]
    fn bundled_candidates_are_listed_once_each() {
        let (_, candidates) = bundled_agents_dir();
        let unique: std::collections::HashSet<_> = candidates.iter().collect();
        assert_eq!(
            unique.len(),
            candidates.len(),
            "duplicate candidates: {candidates:?}"
        );
    }
}
