//! The binary: a few subcommands, and the server.
//!
//! Everything but `serve` exists because of one failure shape. A misconfigured
//! path produces a server that starts, reports healthy, and cannot do its job —
//! and the symptom surfaces layers away as an empty dropdown or a dead keystroke.
//! `doctor` collapses that to one command, and `migrate` exists so moving an
//! existing install is a supported operation rather than a `cp` a user has to get
//! right.
//!
//! Argument parsing is hand-rolled. Four subcommands with one flag each does not
//! earn a dependency, and every one of them is meant to be typed rarely and read
//! often — so the help text is the interface, not the parser.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_rustls::{rustls, TlsAcceptor};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use sylph_server::{
    api,
    config::{report::Report, resolve, Config, Resolved},
    router, AppState,
};

const USAGE: &str = "\
sylph-server — the Sylph server

USAGE
  sylph-server [serve]              Start the server (default)
  sylph-server doctor               Show what configuration resolved to
  sylph-server config init          Create missing config.toml and SOUL.md
  sylph-server config path          Print the config file's path
  sylph-server migrate --from DIR   Copy an existing data directory in

Everything Sylph owns lives under one root, ~/.sylph by default. Point
SYLPH_HOME elsewhere to move the whole install. See docs/configuration.md.
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None | Some("serve") => serve().await,
        Some("doctor") => doctor(),
        Some("config") => config_command(&args[1..]),
        Some("migrate") => migrate(&args[1..]),
        Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("--version") | Some("-V") => {
            println!("sylph-server {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprint!("Unknown command: {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

/// Resolve configuration without seeding, for the commands that only report.
///
/// `doctor` must describe the install as it *is*, including the absence of a
/// config file. Seeding first would make it impossible to run once and learn that
/// nothing had been set up yet.
fn resolved_only() -> anyhow::Result<Resolved> {
    Resolved::load()
}

fn report_for(resolved: &Resolved) -> Report {
    Report::build(resolved)
}

/// Print what resolved, then say whether the pieces it names are usable.
fn doctor() -> anyhow::Result<()> {
    let resolved = resolved_only()?;
    print!("{}", report_for(&resolved).render());

    println!("\nAgent availability");
    println!("  Reported per paired environment by `GET /api/v1/environments`.");

    // Three states, not two: a resolved file, a path with nothing at it, and
    // identity declined outright. The last is a supported configuration rather than
    // a problem, so it reads as a statement instead of a warning.
    let identity = match resolved.soul.value.as_ref() {
        Some(path) if path.is_file() => {
            format!("{} will be injected into Sylph sessions", path.display())
        }
        Some(path) => format!(
            "none at {} — every session runs on the agent's own identity",
            path.display()
        ),
        None => "declined in configuration — every session runs on the agent's own \
                 identity"
            .to_string(),
    };
    println!("Identity         {identity}");

    Ok(())
}

fn config_command(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("path") => {
            println!("{}", resolved_only()?.config_path.display());
            Ok(())
        }
        // The same seeding a first run performs, plus `SOUL.md` unconditionally.
        // A restart deliberately does *not* restore a deleted identity file, since
        // deleting it means "no identity"; this is the explicit way to ask for it
        // back.
        None | Some("init") => {
            let resolved = resolved_only()?;
            let mut notes = sylph_server::config::seed(&resolved)?;
            notes.extend(sylph_server::config::seed_soul(&resolved)?);
            if notes.is_empty() {
                println!(
                    "Nothing to do — {} is already set up.",
                    resolved.home.display()
                );
            } else {
                for note in notes {
                    println!("{note}");
                }
            }
            Ok(())
        }
        Some(other) => {
            eprint!("Unknown config command: {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

/// Copy an existing data directory into the configured one.
///
/// Takes an explicit `--from` rather than probing known former locations. A probe
/// would make the old layout a fallback that has to keep working, which is exactly
/// what this whole change removes; and a user who moved their data somewhere of
/// their own choosing is served by the explicit form and not by a guess.
fn migrate(args: &[String]) -> anyhow::Result<()> {
    let mut from: Option<PathBuf> = None;
    let mut force = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--from" => {
                from = rest.next().map(PathBuf::from);
            }
            "--force" => force = true,
            other => anyhow::bail!("Unknown migrate option: {other}\n\n{USAGE}"),
        }
    }

    let Some(from) = from else {
        anyhow::bail!("migrate needs --from DIR\n\n{USAGE}");
    };
    let resolved = resolved_only()?;
    let from = resolve::expand_tilde(
        &from.display().to_string(),
        &resolved.user_home.to_string_lossy(),
    );
    let to = &resolved.data_dir.value;

    if !from.is_dir() {
        anyhow::bail!("{} is not a directory", from.display());
    }
    if from == *to {
        println!("{} is already the data directory.", from.display());
        return Ok(());
    }

    // Refuse rather than merge. Two journals cannot be combined — session ids are
    // unique per store and the blob index would reference rows that are not there
    // — so a silent overwrite would destroy whichever one the user cared about.
    if to.join("journal.db").exists() && !force {
        anyhow::bail!(
            "{} already holds a journal. Move it aside, or pass --force to replace it.",
            to.display()
        );
    }

    std::fs::create_dir_all(to)?;
    let mut copied = Vec::new();
    // Named rather than globbed, so a stray file in an old data directory is not
    // carried into the new one. The `-wal` and `-shm` files come along because a
    // journal copied without them can lose the tail of its last transaction.
    for name in [
        "journal.db",
        "journal.db-wal",
        "journal.db-shm",
        "history.db",
        "history.db-wal",
        "history.db-shm",
    ] {
        let source = from.join(name);
        if source.is_file() {
            std::fs::copy(&source, to.join(name))?;
            copied.push(name.to_string());
        }
    }

    let blobs = from.join("blobs");
    if blobs.is_dir() {
        let count = copy_tree(&blobs, &to.join("blobs"))?;
        copied.push(format!("blobs/ ({count} blobs)"));
    }

    let attachments = from.join("attachments");
    if attachments.is_dir() {
        let count = copy_tree(&attachments, &to.join("attachments"))?;
        copied.push(format!("attachments/ ({count} files)"));
    }

    if copied.is_empty() {
        anyhow::bail!(
            "Nothing to migrate: {} holds no journal, history, blobs, or attachments.",
            from.display()
        );
    }

    println!("Migrated into {}:", to.display());
    for item in copied {
        println!("  {item}");
    }
    println!("\nThe original at {} was not modified.", from.display());
    Ok(())
}

/// Copy a directory tree, returning how many files were written.
fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut count = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            count += copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
            count += 1;
        }
    }
    Ok(count)
}

async fn serve() -> anyhow::Result<()> {
    // Resolved before logging is initialized, because the log level is itself a
    // setting. A config error therefore has to be reported without tracing, which
    // is why this arm prints rather than logs.
    let resolved = match Resolved::load() {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("Configuration error: {error:#}");
            std::process::exit(1);
        }
    };

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            resolved.log_level.value.clone(),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let seeded = sylph_server::config::seed(&resolved)?;
    let first_run = !seeded.is_empty();
    for note in &seeded {
        tracing::info!("{note}");
    }
    let config = Config::from_resolved(&resolved);

    api::health::mark_start();
    tracing::info!(
        "Sylph home {} (config {})",
        config.home.display(),
        config.config_path.display()
    );
    // The whole resolution, once, at startup. Every failure this module guards
    // against is diagnosable from these lines, and asking a user to run a second
    // command to get them is asking them to already suspect configuration.
    if first_run {
        tracing::info!(
            "First run. Edit {} to change anything below.",
            config.config_path.display()
        );
    }
    for line in report_for(&resolved).render().lines() {
        tracing::debug!("{line}");
    }

    let host = config.host.clone();
    let port = config.port;
    let state = AppState::build(config).await?;

    // The archive-availability line that used to sit here is gone with the separate
    // store. There is no external binary whose presence could be reported: history is
    // `history.db` beside the journal, which either opened during `AppState::build` or
    // failed the startup outright. `AppState::build` logs which agents this build can
    // run, which is the fact that replaced it — and `GET /api/v1/environments` is where
    // per-machine availability is reported (`docs/environments.md`).

    let env_bridge = state.env_bridge.clone();
    let app = router(state);

    // The envagent ingress is always bound. A single-machine install therefore
    // has two processes, and an unpaired server has no execution path.
    {
        let default = format!("127.0.0.1:{}", port + 1);
        let raw = std::env::var("SYLPH_ENV_LISTEN").unwrap_or(default);
        let listener = tokio::net::TcpListener::bind(&raw).await?;
        let local_addr = listener.local_addr()?;
        let tls = tls_acceptor_from_env()?;
        if !local_addr.ip().is_loopback() && tls.is_none() {
            anyhow::bail!(
                "SYLPH_ENV_LISTEN resolved to {local_addr}, but the development envagent \
                 transport is plaintext and must remain loopback-only; set \
                 SYLPH_ENV_TLS_CERT and SYLPH_ENV_TLS_KEY before exposing it on a network interface"
            );
        }
        tracing::info!("Envagent listener on {}", local_addr);
        tokio::spawn(async move {
            let result = match tls {
                Some(acceptor) => env_bridge.serve_tls(listener, acceptor).await,
                None => env_bridge.serve(listener).await,
            };
            if let Err(error) = result {
                tracing::error!("Envagent listener stopped: {error:#}");
            }
        });
    }

    // The cookie/bootstrap model is intentionally process-local.  Refuse a
    // non-loopback bind until a separate authenticated/TLS deployment mode
    // exists; warning while exposing a bearer bootstrap endpoint would make the
    // security boundary aspirational rather than enforced.
    let ip: IpAddr = host
        .parse()
        .map_err(|_| anyhow::anyhow!("`server.host` is not an IP address: {host}"))?;
    if !ip.is_loopback() {
        anyhow::bail!(
            "`server.host` resolved to {ip}, but network-reachable deployment is disabled: \
             configure loopback or add the authenticated/TLS deployment mode"
        );
    }
    let addr = SocketAddr::new(ip, port);

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        // A hand-started local server hits this constantly: an instance from an
        // earlier run is still holding the port. The bare OS message ("Address
        // already in use (os error 48)") does not say which port or what to do.
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::anyhow!(
                "Port {port} is already in use. Another Sylph backend is probably still \
                 running — check with `lsof -ti :{port}`, then stop it or start this one \
                 on a different port with SYLPH_PORT."
            )
        } else {
            anyhow::anyhow!("Could not bind {addr}: {e}")
        }
    })?;
    tracing::info!("Listening on http://{}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}

fn tls_acceptor_from_env() -> anyhow::Result<Option<TlsAcceptor>> {
    let cert_path = std::env::var("SYLPH_ENV_TLS_CERT").ok();
    let key_path = std::env::var("SYLPH_ENV_TLS_KEY").ok();
    match (cert_path, key_path) {
        (None, None) => Ok(None),
        (Some(cert_path), Some(key_path)) => {
            let cert_bytes = std::fs::read(&cert_path)?;
            let key_bytes = std::fs::read(&key_path)?;
            let certs = rustls_pemfile::certs(&mut std::io::Cursor::new(cert_bytes))
                .collect::<Result<Vec<_>, _>>()?;
            let mut key_reader = std::io::Cursor::new(key_bytes);
            let mut keys = rustls_pemfile::pkcs8_private_keys(&mut key_reader);
            let key = keys
                .next()
                .ok_or_else(|| anyhow::anyhow!("TLS key file contains no PKCS#8 private key"))??;
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key.into())?;
            Ok(Some(TlsAcceptor::from(Arc::new(config))))
        }
        _ => anyhow::bail!("SYLPH_ENV_TLS_CERT and SYLPH_ENV_TLS_KEY must be set together"),
    }
}
