//! Command-line entry point for the environment agent.

use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;
use sylph_protocol::{EnvironmentHealth, Hello, ProducerId};

use sylph_envagent::{CredentialStore, HealthReporter};

const USAGE: &str = "\
sylph-envagent — run Sylph adapters on this machine

USAGE
  sylph-envagent doctor
  sylph-envagent pair --server URL --code CODE
  sylph-envagent invite --capability observe|control --ttl 15m
  sylph-envagent run
";

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(feature = "e2e")]
    eprintln!("Fixture adapters enabled: codex (no vendor processes)");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("doctor") | None => doctor().await,
        Some("pair") => pair(&args[1..]).await,
        Some("invite") => invite(&args[1..]).await,
        Some("run") => run(&args[1..]).await,
        Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command '{other}'\n\n{USAGE}"),
    }
}

fn environment_home() -> PathBuf {
    std::env::var_os("SYLPH_ENV_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".sylph-env")))
        .unwrap_or_else(|| PathBuf::from(".sylph-env"))
}

async fn doctor() -> Result<()> {
    let home = environment_home();
    let credential = CredentialStore::new(home.join("credential.json")).load()?;
    println!("Environment home {}", home.display());
    match credential {
        Some(ref credential) => println!(
            "Paired environment {} with server {}",
            credential.environment_id, credential.server
        ),
        None => println!("Not paired"),
    }
    let environment_id = credential
        .as_ref()
        .map(|credential| credential.environment_id.clone())
        .unwrap_or_else(|| "unpaired".into());
    let health = HealthReporter::new().snapshot(environment_id).await;
    for agent in health.agents {
        match (agent.available, agent.version) {
            (true, Some(version)) => println!("  {} {version}", agent.name),
            (true, None) => println!("  {} available", agent.name),
            (false, _) => println!(
                "  {} unavailable ({})",
                agent.name,
                agent.reason.unwrap_or_else(|| "unknown reason".into())
            ),
        }
    }
    Ok(())
}

async fn pair(args: &[String]) -> Result<()> {
    let mut server = None;
    let mut code = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--server" => server = iter.next().cloned(),
            "--code" => code = iter.next().cloned(),
            other => anyhow::bail!("unknown pair option '{other}'"),
        }
    }
    let server = server.ok_or_else(|| anyhow::anyhow!("pair needs --server URL"))?;
    let code = code.ok_or_else(|| anyhow::anyhow!("pair needs --code CODE"))?;
    let body = serde_json::json!({
        "code": code,
        "name": std::env::var("SYLPH_ENV_NAME").unwrap_or_else(|_| "Environment".into()),
        "agents": []
    });
    let response: PairingResponse =
        http_post_json(&server, "/api/v1/environments/pair", &body).await?;
    let store = CredentialStore::new(environment_home().join("credential.json"));
    let environment_id = response.environment_id.clone();
    // Pairing talks to the HTTP API, while the envagent stream uses the
    // development wire listener (the adjacent default port). Operators with
    // a custom listener can override this with SYLPH_ENV_SERVER; never persist
    // the HTTP endpoint as if it were a framed envagent socket.
    let wire_server = std::env::var("SYLPH_ENV_SERVER")
        .unwrap_or_else(|_| wire_server_for_pairing(&server).unwrap_or_else(|_| server.clone()));
    store.save(&sylph_envagent::Credential {
        environment_id,
        installation_id: Some(response.installation_id.clone()),
        token: response.credential,
        server: wire_server,
    })?;
    println!("Paired environment {}", response.environment_id);
    Ok(())
}

async fn invite(args: &[String]) -> Result<()> {
    let mut capability = None;
    let mut ttl = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--capability" => capability = iter.next().cloned(),
            "--ttl" => ttl = iter.next().cloned(),
            other => anyhow::bail!("unknown invite option '{other}'"),
        }
    }
    let capability = capability.ok_or_else(|| anyhow::anyhow!("invite needs --capability"))?;
    if !matches!(capability.as_str(), "observe" | "control") {
        anyhow::bail!("invite capability must be observe or control");
    }
    let ttl_ms = parse_duration_ms(ttl.as_deref().unwrap_or("15m"))?;
    let credential = CredentialStore::new(environment_home().join("credential.json"))
        .load()?
        .ok_or_else(|| anyhow::anyhow!("not paired; run sylph-envagent pair first"))?;
    let path = format!("/api/v1/environments/{}/invites", credential.environment_id);
    let body = serde_json::json!({
        "installationId": credential.installation_id.clone().unwrap_or_else(|| credential.environment_id.clone()),
        "token": credential.token,
        "capability": capability,
        "ttlMs": ttl_ms,
    });
    let response: InviteResponse = http_post_json(
        &http_server_for_credential(&credential.server)?,
        &path,
        &body,
    )
    .await?;
    println!("{} (expires {})", response.code, response.expires_at_ms);
    Ok(())
}

fn parse_duration_ms(value: &str) -> Result<i64> {
    let (number, unit) = value.split_at(value.len().saturating_sub(1));
    let amount: i64 = number
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid duration '{value}'"))?;
    if amount <= 0 {
        anyhow::bail!("duration must be positive");
    }
    let multiplier = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => anyhow::bail!("duration must end in s, m, or h"),
    };
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow::anyhow!("duration is too large"))
}

async fn run(_args: &[String]) -> Result<()> {
    let credential = CredentialStore::new(environment_home().join("credential.json"))
        .load()?
        .ok_or_else(|| anyhow::anyhow!("not paired; run sylph-envagent pair first"))?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is required for the envagent"))?;
    let extra_roots = std::env::var("SYLPH_BROWSE_ROOTS")
        .unwrap_or_default()
        .split(':')
        .filter(|root| !root.trim().is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let path_policy = sylph_envagent::PathPolicy::new(&home, extra_roots)
        .map_err(|error| anyhow::anyhow!(error))?;
    let health = HealthReporter::new()
        .snapshot(credential.environment_id.clone())
        .await;
    let agents = health.agents.clone();
    let hello = Hello {
        protocol: sylph_protocol::ENV_PROTOCOL.into(),
        versions: vec![sylph_protocol::ProtocolVersion::CURRENT],
        environment_id: Some(sylph_protocol::EnvironmentId::parse(
            credential.environment_id.clone(),
        )?),
        installation_id: credential
            .installation_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| credential.environment_id.clone()),
        producer_id: ProducerId::parse(
            credential
                .installation_id
                .clone()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| credential.environment_id.clone()),
        )?,
        connection_epoch: 0,

        agents,
        health: Some(EnvironmentHealth {
            connected: true,
            agents: health.agents.clone(),
            facts: health.facts.clone(),
        }),
    };
    let config = sylph_envagent::EnvAgentConfig {
        environment_id: credential.environment_id.clone(),
        producer_id: credential
            .installation_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| credential.environment_id.clone()),
        spool: sylph_envagent::SpoolConfig::new(
            environment_home().join("spool.db"),
            std::env::var("SYLPH_ENV_SPOOL_MAX")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(512 * 1024 * 1024),
        ),
        path_policy,
    };
    let server = std::env::var("SYLPH_ENV_SERVER").unwrap_or(credential.server);
    sylph_envagent::run_runtime_reconnecting(server, hello, credential.token, config).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingResponse {
    environment_id: String,
    #[serde(default)]
    installation_id: String,
    credential: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InviteResponse {
    code: String,
    expires_at_ms: i64,
}

async fn http_post_json<T: serde::de::DeserializeOwned>(
    base: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<T> {
    let secure = base.starts_with("https://");
    let (host, port) = parse_http_authority(base)?;
    let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
    if secure {
        let ca_path = std::env::var("SYLPH_ENV_CA_CERT")
            .map_err(|_| anyhow::anyhow!("HTTPS pairing requires SYLPH_ENV_CA_CERT"))?;
        let ca = std::fs::read(ca_path)?;
        let tls = sylph_envagent::tls::connect_stream(tcp, &host, &ca).await?;
        return http_post_on_stream(tls, &host, port, path, body).await;
    } else {
        return http_post_on_stream(tcp, &host, port, path, body).await;
    }
}

async fn http_post_on_stream<
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: serde::de::DeserializeOwned,
>(
    mut stream: S,
    host: &str,
    port: u16,
    path: &str,
    body: &serde_json::Value,
) -> Result<T> {
    let bytes = serde_json::to_vec(body)?;
    let host_header = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_header}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    use tokio::io::AsyncWriteExt;
    stream.write_all(request.as_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    use tokio::io::AsyncReadExt;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("server response had no header terminator"))?;
    let header = std::str::from_utf8(&response[..split])?;
    if !header.starts_with("HTTP/1.1 201") && !header.starts_with("HTTP/1.0 201") {
        anyhow::bail!(
            "pairing request failed: {}",
            header.lines().next().unwrap_or(header)
        );
    }
    Ok(serde_json::from_slice(&response[split + 4..])?)
}

fn http_server_for_credential(server: &str) -> Result<String> {
    if server.starts_with("http://") || server.starts_with("https://") {
        return Ok(server.to_string());
    }
    let authority = format!("http://{server}");
    let (host, port) = parse_http_authority(&authority)?;
    let http_port = port
        .checked_sub(1)
        .ok_or_else(|| anyhow::anyhow!("envagent wire port cannot be decremented"))?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    Ok(format!("http://{host}:{http_port}"))
}

fn parse_http_authority(base: &str) -> Result<(String, u16)> {
    let secure = base.starts_with("https://");
    let authority = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .ok_or_else(|| anyhow::anyhow!("pairing server must use http:// or https://"))?
        .split('/')
        .next()
        .unwrap_or_default();
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, suffix) = rest
            .split_once(']')
            .ok_or_else(|| anyhow::anyhow!("pairing server has an invalid IPv6 host"))?;
        let port = if suffix.is_empty() {
            4317
        } else {
            suffix
                .strip_prefix(':')
                .ok_or_else(|| anyhow::anyhow!("pairing server has an invalid port"))?
                .parse::<u16>()?
        };
        (host.to_string(), port)
    } else if authority.matches(':').count() == 1 {
        let (host, port) = authority
            .split_once(':')
            .expect("one colon was counted above");
        (host.to_string(), port.parse::<u16>()?)
    } else if authority.contains(':') {
        anyhow::bail!("pairing server IPv6 hosts must be bracketed")
    } else {
        (authority.to_string(), 4317)
    };
    if host.is_empty() {
        anyhow::bail!("pairing server has no host");
    }
    if !secure && !is_loopback_host(&host) {
        anyhow::bail!(
            "pairing server '{host}' is not loopback; plaintext envagent transport requires TLS"
        );
    }
    Ok((host, port))
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn wire_server_for_pairing(base: &str) -> Result<String> {
    let (host, port) = parse_http_authority(base)?;
    let wire_port = port
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("pairing server port cannot be incremented"))?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let scheme = if base.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    Ok(format!("{scheme}://{host}:{wire_port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_http_authority_defaults_to_the_backend_port() {
        assert_eq!(
            parse_http_authority("http://127.0.0.1").unwrap(),
            ("127.0.0.1".into(), 4317)
        );
        assert_eq!(
            wire_server_for_pairing("http://127.0.0.1:4317").unwrap(),
            "http://127.0.0.1:4318"
        );
        assert_eq!(
            wire_server_for_pairing("http://[::1]").unwrap(),
            "http://[::1]:4318"
        );
    }

    #[test]
    fn pairing_rejects_malformed_urls_but_accepts_secure_urls() {
        assert_eq!(
            parse_http_authority("https://127.0.0.1:4317").unwrap(),
            ("127.0.0.1".into(), 4317)
        );
        assert!(parse_http_authority("http://::1:4317").is_err());
        assert!(parse_http_authority("http://127.0.0.1:not-a-port").is_err());
    }
}
