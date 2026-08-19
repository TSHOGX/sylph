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

        // Generate random cookie secret on startup
        let cookie_secret = uuid::Uuid::new_v4().to_string();

        Ok(Self {
            host,
            port,
            database_path,
            data_dir,
            runners_dir,
            tracedb_bin,
            cookie_secret,
        })
    }
}
