//! Length-delimited transport primitives for sylph.env.v1.

use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};
use std::pin::Pin;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Hard upper bound for one framed message.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame is empty")]
    Empty,
    #[error("frame is {actual} bytes, above the {maximum}-byte limit")]
    TooLarge { actual: usize, maximum: usize },
    #[error("I/O while reading or writing a frame: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON in frame: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid envagent wire message: {0}")]
    Wire(#[from] sylph_protocol::WireError),
    #[error("invalid envagent server address: {0}")]
    Address(String),
}

/// A connection carrying one length-delimited JSON message at a time.
///
/// The framing is deliberately independent of vendor stdio framing. A single
/// connection can carry commands, event batches, heartbeats, and diagnostics;
/// bounded frames provide backpressure before a payload reaches an adapter.
pub struct FramedConnection<S> {
    stream: S,
    max_frame_bytes: usize,
}

impl<S> FramedConnection<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            max_frame_bytes: MAX_FRAME_BYTES,
        }
    }

    pub fn with_max_frame_bytes(mut self, max_frame_bytes: usize) -> Self {
        self.max_frame_bytes = max_frame_bytes.max(1);
        self
    }

    pub fn into_inner(self) -> S {
        self.stream
    }
}

impl<S> FramedConnection<S>
where
    S: AsyncRead + Unpin,
{
    pub async fn read_bytes(&mut self) -> Result<Vec<u8>, FrameError> {
        let length = self.stream.read_u32().await? as usize;
        if length == 0 {
            return Err(FrameError::Empty);
        }
        if length > self.max_frame_bytes {
            return Err(FrameError::TooLarge {
                actual: length,
                maximum: self.max_frame_bytes,
            });
        }
        let mut payload = vec![0_u8; length];
        self.stream.read_exact(&mut payload).await?;
        Ok(payload)
    }
}

impl<S> FramedConnection<S>
where
    S: AsyncWrite + Unpin,
{
    pub async fn write_bytes(&mut self, payload: &[u8]) -> Result<(), FrameError> {
        if payload.is_empty() {
            return Err(FrameError::Empty);
        }
        if payload.len() > self.max_frame_bytes {
            return Err(FrameError::TooLarge {
                actual: payload.len(),
                maximum: self.max_frame_bytes,
            });
        }
        self.stream.write_u32(payload.len() as u32).await?;
        self.stream.write_all(payload).await?;
        self.stream.flush().await?;
        Ok(())
    }

    pub async fn write_json<T: Serialize>(&mut self, value: &T) -> Result<(), FrameError> {
        let payload = serde_json::to_vec(value)?;
        self.write_bytes(&payload).await
    }
}

impl<S> FramedConnection<S>
where
    S: AsyncRead + Unpin,
{
    pub async fn read_json<T: DeserializeOwned>(&mut self) -> Result<T, FrameError> {
        Ok(serde_json::from_slice(&self.read_bytes().await?)?)
    }

    pub async fn read_env_message(&mut self) -> Result<sylph_protocol::DecodedMessage, FrameError> {
        Ok(sylph_protocol::decode_message(&self.read_bytes().await?)?)
    }
}

impl<S> FramedConnection<S>
where
    S: AsyncWrite + Unpin,
{
    pub async fn write_env_message(
        &mut self,
        message: &sylph_protocol::WireMessage,
    ) -> Result<(), FrameError> {
        self.write_json(message).await
    }
}

/// Monotonic reconnect epoch. It is incremented for every newly established
/// connection and included in hello/welcome so a stale writer cannot ack a new
/// link's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ConnectionEpoch(pub u64);

impl ConnectionEpoch {
    pub fn advance(&mut self) -> Self {
        self.0 = self.0.saturating_add(1);
        *self
    }
}

/// Exponential reconnect delay with a bounded ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    initial: Duration,
    maximum: Duration,
    current: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(100), Duration::from_secs(30))
    }
}

impl Backoff {
    pub fn new(initial: Duration, maximum: Duration) -> Self {
        let initial = initial.max(Duration::from_millis(1));
        let maximum = maximum.max(initial);
        Self {
            initial,
            maximum,
            current: initial,
        }
    }

    pub fn delay(&self) -> Duration {
        self.current
    }

    pub fn reset(&mut self) {
        self.current = self.initial;
    }

    pub fn fail(&mut self) -> Duration {
        let delay = self.current;
        self.current = self
            .current
            .checked_mul(2)
            .unwrap_or(self.maximum)
            .min(self.maximum);
        delay
    }
}

/// Outbound TCP connection supervisor used by the local development
/// deployment. TLS termination can wrap the stream later without changing
/// framing or reconnect semantics.
pub struct ConnectionSupervisor {
    address: String,
    backoff: Backoff,
    epoch: ConnectionEpoch,
    credential: Option<String>,
}

pub trait WireIo: AsyncRead + AsyncWrite + Send {}
impl<T: AsyncRead + AsyncWrite + Send> WireIo for T {}
type WireStream = Pin<Box<dyn WireIo + Unpin>>;

impl ConnectionSupervisor {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            backoff: Backoff::default(),
            epoch: ConnectionEpoch(0),
            credential: None,
        }
    }

    pub fn with_credential(mut self, credential: impl Into<String>) -> Self {
        self.credential = Some(credential.into());
        self
    }

    pub fn epoch(&self) -> ConnectionEpoch {
        self.epoch
    }

    pub async fn connect(
        &mut self,
        hello: &sylph_protocol::Hello,
    ) -> Result<FramedConnection<WireStream>, FrameError> {
        let tls = self.address.starts_with("https://");
        let address = wire_address(&self.address).map_err(FrameError::Address)?;
        let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(address))
            .await
            .map_err(|_| {
                FrameError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "envagent connection timed out",
                ))
            })??;
        // Length and body are separate writes. Capture upload/receipt RPCs wait for
        // each other, so Nagle plus delayed ACK otherwise adds a timer per frame.
        stream.set_nodelay(true)?;
        self.epoch = self.epoch.advance();
        let stream = if tls {
            let ca = std::env::var("SYLPH_ENV_CA_CERT").map_err(|_| {
                FrameError::Address("https envagent transport requires SYLPH_ENV_CA_CERT".into())
            })?;
            let pem = std::fs::read(ca)
                .map_err(|e| FrameError::Address(format!("cannot read CA certificate: {e}")))?;
            // Preserve an IPv6 literal as the TLS server name.  Splitting on
            // `:` first turns `[::1]` into an empty string and makes every
            // IPv6 HTTPS reconnect fail before the handshake starts.
            let authority = self
                .address
                .trim_start_matches("https://")
                .split('/')
                .next()
                .unwrap_or("");
            let host = if let Some(rest) = authority.strip_prefix('[') {
                rest.split_once(']').map(|(host, _)| host).unwrap_or("")
            } else {
                authority
                    .rsplit_once(':')
                    .filter(|(host, port)| !host.contains(':') && port.parse::<u16>().is_ok())
                    .map(|(host, _)| host)
                    .unwrap_or(authority)
            };
            let tls = crate::tls::connect_stream(stream, host, &pem)
                .await
                .map_err(|e| FrameError::Address(format!("TLS handshake failed: {e}")))?;
            Box::pin(tls) as WireStream
        } else {
            Box::pin(stream) as WireStream
        };
        let mut framed = FramedConnection::new(stream);
        let mut hello = hello.clone();
        hello.connection_epoch = self.epoch.0;
        framed
            .write_env_message(&sylph_protocol::WireMessage::from_hello(hello))
            .await?;
        if let Some(credential) = &self.credential {
            framed.write_json(credential).await?;
        }
        Ok(framed)
    }

    pub async fn connect_and_welcome(
        &mut self,
        hello: &sylph_protocol::Hello,
    ) -> anyhow::Result<(FramedConnection<WireStream>, sylph_protocol::Welcome)> {
        let mut framed = self.connect(hello).await?;
        let welcome = handshake_welcome(&mut framed).await?;
        self.backoff.reset();
        Ok((framed, welcome))
    }

    /// Validate a welcome on an already framed stream. Kept generic so
    /// deterministic tests can use an in-memory duplex and so TLS wrappers
    /// can reuse the same handshake.
    pub async fn handshake_stream<S>(
        framed: &mut FramedConnection<S>,
    ) -> anyhow::Result<sylph_protocol::Welcome>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        handshake_welcome(framed).await
    }

    /// Retry a connection until the callback returns Ok. A callback error
    /// leaves all unacknowledged spool rows intact for the next iteration.
    pub async fn run<F, Fut>(
        &mut self,
        hello: &sylph_protocol::Hello,
        mut on_message: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(sylph_protocol::DecodedMessage) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        loop {
            match self.connect_and_welcome(hello).await {
                Ok((mut connection, _welcome)) => loop {
                    match connection.read_env_message().await {
                        Ok(message) => on_message(message).await?,
                        Err(error) => {
                            tracing::debug!("envagent connection ended: {error}");
                            break;
                        }
                    }
                },
                Err(error) => {
                    let delay = self.backoff.fail();
                    tokio::time::sleep(delay).await;
                    tracing::debug!("envagent reconnect after {delay:?}: {error}");
                }
            }
        }
    }
}

fn wire_address(raw: &str) -> Result<String, String> {
    let raw = if let Some(raw) = raw.strip_prefix("http://") {
        raw
    } else if raw.starts_with("https://") {
        let raw = raw.trim_start_matches("https://");
        return parse_tls_address(raw);
    } else {
        raw
    };
    let authority = raw.split('/').next().unwrap_or(raw);
    if authority.is_empty() {
        return Err("address is empty".into());
    }
    if authority.starts_with('[') {
        let closing = authority
            .find(']')
            .ok_or_else(|| "IPv6 address is missing a closing bracket".to_string())?;
        if authority[closing + 1..].is_empty() {
            if !is_loopback_host(&authority[1..closing]) {
                return Err("plaintext envagent transport requires a loopback host".into());
            }
            return Ok(format!("{authority}:4318"));
        }
        if !authority[closing + 1..].starts_with(':') {
            return Err("IPv6 address has invalid authority suffix".into());
        }
        authority[closing + 2..]
            .parse::<u16>()
            .map_err(|_| "IPv6 port is not numeric".to_string())?;
        if !is_loopback_host(&authority[1..closing]) {
            return Err("plaintext envagent transport requires a loopback host".into());
        }
        return Ok(authority.to_string());
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            if host.is_empty() {
                return Err("address has no host".into());
            }
            port.parse::<u16>()
                .map_err(|_| "port is not numeric".to_string())?;
            if !is_loopback_host(host) {
                return Err("plaintext envagent transport requires a loopback host".into());
            }
            Ok(authority.to_string())
        }
        Some(_) => Err("bare IPv6 addresses must be bracketed".into()),
        None => {
            if !is_loopback_host(authority) {
                return Err("plaintext envagent transport requires a loopback host".into());
            }
            Ok(format!("{authority}:4318"))
        }
    }
}

fn parse_tls_address(raw: &str) -> Result<String, String> {
    let authority = raw.split('/').next().unwrap_or(raw);
    if authority.is_empty() {
        return Err("address is empty".into());
    }
    if authority.starts_with('[') {
        let closing = authority
            .find(']')
            .ok_or("IPv6 address is missing a closing bracket")?;
        if authority[closing + 1..].is_empty() {
            return Ok(format!("{authority}:4318"));
        }
        authority[closing + 2..]
            .parse::<u16>()
            .map_err(|_| "port is not numeric".to_string())?;
        return Ok(authority.to_string());
    }
    if authority.contains(':') {
        let (_, port) = authority.rsplit_once(':').ok_or("invalid address")?;
        port.parse::<u16>()
            .map_err(|_| "port is not numeric".to_string())?;
        Ok(authority.to_string())
    } else {
        Ok(format!("{authority}:4318"))
    }
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

async fn handshake_welcome<S>(
    framed: &mut FramedConnection<S>,
) -> anyhow::Result<sylph_protocol::Welcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let message = framed.read_env_message().await?;
    match message {
        sylph_protocol::DecodedMessage::Known(sylph_protocol::WireMessage::Welcome {
            protocol,
            version,
            environment_id,
            connection_epoch,
            server_epoch,

            heartbeat_interval_ms,
        }) => {
            if protocol != sylph_protocol::ENV_PROTOCOL {
                anyhow::bail!("server selected unsupported protocol {protocol}");
            }
            Ok(sylph_protocol::Welcome {
                protocol,
                version,
                environment_id,
                connection_epoch,
                server_epoch,

                heartbeat_interval_ms,
            })
        }
        sylph_protocol::DecodedMessage::Unknown { kind, .. } => {
            anyhow::bail!("server did not send welcome (got {kind})")
        }
        sylph_protocol::DecodedMessage::Known(other) => {
            anyhow::bail!("server did not send welcome (got {})", other.kind())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn fragmented_duplex_round_trips_json() {
        let (left, mut right) = duplex(1024);
        let mut writer = FramedConnection::new(left);
        let send = tokio::spawn(async move {
            writer
                .write_json(&serde_json::json!({"kind": "hello"}))
                .await
        });
        // The peer sees the four-byte length and payload as one logical frame,
        // regardless of how the underlying duplex implementation chunks writes.
        let mut reader = FramedConnection::new(&mut right);
        let value: serde_json::Value = reader.read_json().await.unwrap();
        send.await.unwrap().unwrap();
        assert_eq!(value["kind"], "hello");
    }

    #[tokio::test]
    async fn oversized_frames_are_refused_before_allocation() {
        let (left, mut right) = duplex(32);
        right.write_u32(1024).await.unwrap();
        let mut reader = FramedConnection::new(left).with_max_frame_bytes(16);
        assert!(matches!(
            reader.read_bytes().await,
            Err(FrameError::TooLarge {
                actual: 1024,
                maximum: 16
            })
        ));
    }

    #[test]
    fn backoff_is_bounded_and_resets_after_success() {
        let mut backoff = Backoff::new(Duration::from_millis(2), Duration::from_millis(5));
        assert_eq!(backoff.fail(), Duration::from_millis(2));
        assert_eq!(backoff.fail(), Duration::from_millis(4));
        assert_eq!(backoff.fail(), Duration::from_millis(5));
        assert_eq!(backoff.delay(), Duration::from_millis(5));
        backoff.reset();
        assert_eq!(backoff.delay(), Duration::from_millis(2));
    }

    #[test]
    fn server_urls_are_normalized_to_the_wire_authority() {
        assert_eq!(
            wire_address("http://127.0.0.1:4318/api").unwrap(),
            "127.0.0.1:4318"
        );
        assert_eq!(wire_address("127.0.0.1").unwrap(), "127.0.0.1:4318");
        assert_eq!(wire_address("http://[::1]").unwrap(), "[::1]:4318");
        assert_eq!(wire_address("[::1]:4319").unwrap(), "[::1]:4319");
        assert_eq!(
            wire_address("https://127.0.0.1:4318").unwrap(),
            "127.0.0.1:4318"
        );
        assert!(wire_address("::1").is_err());
        assert!(wire_address("127.0.0.1:not-a-port").is_err());
    }

    #[tokio::test]
    async fn supervisor_sends_the_credential_after_hello_and_reads_welcome() {
        // Some hermetic runners deny all socket creation, even on loopback.
        // Keep this as an integration check where sockets are available, while
        // allowing the pure framing tests to remain the local unit gate.
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("could not bind loopback test listener: {error}"),
        };
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut framed = FramedConnection::new(stream);
            let hello = framed.read_env_message().await.unwrap();
            assert!(matches!(
                hello,
                sylph_protocol::DecodedMessage::Known(sylph_protocol::WireMessage::Hello { .. })
            ));
            let credential: String = framed.read_json().await.unwrap();
            assert_eq!(credential, "secret");
            framed
                .write_env_message(&sylph_protocol::WireMessage::Welcome {
                    protocol: sylph_protocol::ENV_PROTOCOL.into(),
                    version: sylph_protocol::ProtocolVersion::CURRENT,
                    environment_id: sylph_protocol::EnvironmentId::parse("env7fk2").unwrap(),
                    connection_epoch: 1,
                    server_epoch: 2,

                    heartbeat_interval_ms: 15_000,
                })
                .await
                .unwrap();
        });
        let mut supervisor =
            ConnectionSupervisor::new(address.to_string()).with_credential("secret");
        let hello = sylph_protocol::Hello::current(
            "install",
            sylph_protocol::ProducerId::parse("producer").unwrap(),
            Some(sylph_protocol::EnvironmentId::parse("env7fk2").unwrap()),
            0,
        );
        let (_connection, welcome) = supervisor.connect_and_welcome(&hello).await.unwrap();
        assert_eq!(welcome.connection_epoch, 1);
        server.await.unwrap();
    }
}
