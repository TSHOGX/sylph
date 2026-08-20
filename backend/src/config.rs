use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub database_path: String,
    pub data_dir: PathBuf,
    pub runners_dir: PathBuf,
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
    pub cookie_secret: String,
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

        let runners_dir = env::var("SYLPH_RUNNERS_DIR")
            .unwrap_or_else(|_| format!("{}/.agents/skills/sylph/runners", home))
            .into();

        let tracedb_bin = env::var("TRACEDB_BIN")
            .unwrap_or_else(|_| format!("{}/Workspace/trace-db/target/release/trace-db", home))
            .into();

        let database_path = format!("{}/live.db",
            PathBuf::from(&data_dir).display());

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

        // Generate random cookie secret on startup
        let cookie_secret = uuid::Uuid::new_v4().to_string();

        Ok(Self {
            host,
            port,
            database_path,
            data_dir,
            runners_dir,
            tracedb_bin,
            browse_roots,
            cookie_secret,
        })
    }
}
