//! Tiny process peer used by `tests/envagent_wire.rs`.
//!
//! This binary deliberately contains no backend code.  It speaks only the
//! framed `sylph.env.v1` messages on stdin/stdout, making the integration test
//! exercise the actual OS process boundary while keeping vendor behavior out
//! of the wire test.

use std::env;

use anyhow::{anyhow, Result};
use serde::Serialize;
use sylph_protocol::{
    CommandOutcome, DecodedMessage, EnvironmentCommand, EnvironmentId, Event, EventBatch,
    EventKind, Hello, IdempotencyPolicy, Phase, ProducerId, ProducerSeq, SessionId, WireMessage,
};
use tokio::io::{stdin, stdout, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[tokio::main]
async fn main() -> Result<()> {
    let mode = env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("peer mode missing"))?;
    let environment = EnvironmentId::parse(env::var("SYLPH_WIRE_PEER_ENVIRONMENT")?)?;
    let token = env::var("SYLPH_WIRE_PEER_TOKEN")?;
    let installation = env::var("SYLPH_WIRE_PEER_INSTALLATION")?;
    let producer = ProducerId::parse(env::var("SYLPH_WIRE_PEER_PRODUCER")?)?;
    let workdir = env::var("SYLPH_WIRE_PEER_WORKDIR")?;
    let session_id = SessionId::decode(&env::var("SYLPH_WIRE_PEER_SESSION")?)?;
    let turn_id = env::var("SYLPH_WIRE_PEER_TURN")?;

    let mut reader = stdin();
    let mut writer = stdout();
    write_frame(
        &mut writer,
        &WireMessage::from_hello(Hello::current(
            installation,
            producer.clone(),
            Some(environment.clone()),
            0,
        )),
    )
    .await?;
    write_json_frame(&mut writer, &token).await?;
    let welcome = decode(&read_frame(&mut reader).await?)?;
    let epoch = match welcome {
        DecodedMessage::Known(WireMessage::Welcome {
            connection_epoch, ..
        }) => connection_epoch,
        other => return Err(anyhow!("expected welcome, got {other:?}")),
    };

    match mode.as_str() {
        "promote" => {
            promote(
                &mut reader,
                &mut writer,
                epoch,
                &environment,
                &workdir,
                &session_id,
                &turn_id,
                &producer,
                false,
            )
            .await
        }
        "promote_drop" => {
            promote(
                &mut reader,
                &mut writer,
                epoch,
                &environment,
                &workdir,
                &session_id,
                &turn_id,
                &producer,
                true,
            )
            .await
        }
        "replay" => {
            replay(
                &mut reader,
                &mut writer,
                epoch,
                &environment,
                &session_id,
                &turn_id,
                &producer,
            )
            .await
        }
        "stale" => {
            stale(
                &mut reader,
                &mut writer,
                &environment,
                &session_id,
                &producer,
            )
            .await
        }
        "malformed" => malformed(&mut reader, &mut writer).await,
        other => Err(anyhow!("unknown peer mode {other}")),
    }
}

#[allow(clippy::too_many_arguments)]
async fn promote<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    epoch: u64,
    environment: &EnvironmentId,
    workdir: &str,
    session_id: &SessionId,
    turn_id: &str,
    producer: &ProducerId,
    drop_ack: bool,
) -> Result<()> {
    let mut draft = None;
    let mut prepared = false;
    let mut capabilities = false;
    let mut run = false;
    loop {
        let message = decode(&read_frame(reader).await?)?;
        let DecodedMessage::Known(WireMessage::Command {
            request_id,
            idempotency,
            command,
        }) = message
        else {
            continue;
        };
        let outcome = match command {
            EnvironmentCommand::Prepare { draft_id, .. } => {
                prepared = true;
                draft = Some(draft_id.clone());
                CommandOutcome::accepted_with_result(serde_json::json!({
                    "draftId": draft_id,
                    "workdir": workdir
                }))
            }
            EnvironmentCommand::DraftCapabilities { draft_id } => {
                if draft.as_deref() != Some(draft_id.as_str()) {
                    return Err(anyhow!("capabilities addressed the wrong draft"));
                }
                capabilities = true;
                CommandOutcome::accepted_with_result(serde_json::json!({
                    "capabilities": {"commands": [], "config": []},
                    "supports": ["queue"]
                }))
            }
            EnvironmentCommand::Run {
                draft_id: Some(draft_id),
                initial_turn_id,
                ..
            } => {
                if draft.as_deref() != Some(draft_id.as_str())
                    || initial_turn_id.as_deref() != Some(turn_id)
                {
                    return Err(anyhow!("run lost draft/turn identity"));
                }
                run = true;
                CommandOutcome::accepted_with_result(serde_json::json!({
                    "sessionId": session_id,
                    "workdir": workdir,
                    "fidelity": "stream"
                }))
            }
            EnvironmentCommand::Activate { session_id: id } => {
                if id != *session_id || !prepared || !capabilities || !run {
                    return Err(anyhow!("activate arrived before promotion completed"));
                }
                send_result(
                    writer,
                    request_id,
                    idempotency,
                    epoch,
                    CommandOutcome::accepted(),
                )
                .await?;
                let mut item = sylph_protocol::SequencedEvent::pending(
                    producer.clone(),
                    ProducerSeq::new(1),
                    session_id.clone(),
                    terminal_event(),
                    None,
                );
                item.arrival_turn_id = Some(turn_id.to_string());
                let batch =
                    EventBatch::new(environment.clone(), producer.clone(), epoch, vec![item])?;
                write_frame(writer, &WireMessage::from_event_batch(batch)).await?;
                if drop_ack {
                    // Simulate a network drop after the server may have
                    // committed the event but before this process sees the
                    // acknowledgement. The reconnect peer must replay the
                    // same producer sequence idempotently.
                    return Ok(());
                }
                let ack = decode(&read_frame(reader).await?)?;
                if !matches!(
                    ack,
                    DecodedMessage::Known(WireMessage::Ack { accepted_through, .. })
                        if accepted_through.get() == 1
                ) {
                    return Err(anyhow!("terminal event was not acknowledged"));
                }
                return Ok(());
            }
            other => return Err(anyhow!("unexpected command {}", command_name(&other))),
        };
        send_result(writer, request_id, idempotency, epoch, outcome).await?;
    }
}

async fn replay<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    epoch: u64,
    environment: &EnvironmentId,
    session_id: &SessionId,
    turn_id: &str,
    producer: &ProducerId,
) -> Result<()> {
    let mut item = sylph_protocol::SequencedEvent::pending(
        producer.clone(),
        ProducerSeq::new(1),
        session_id.clone(),
        terminal_event(),
        None,
    );
    item.arrival_turn_id = Some(turn_id.to_string());
    write_frame(
        writer,
        &WireMessage::from_event_batch(EventBatch::new(
            environment.clone(),
            producer.clone(),
            epoch,
            vec![item],
        )?),
    )
    .await?;
    let ack = decode(&read_frame(reader).await?)?;
    if !matches!(
        ack,
        DecodedMessage::Known(WireMessage::Ack { accepted_through, .. })
            if accepted_through.get() == 1
    ) {
        return Err(anyhow!("replay was not acknowledged"));
    }
    Ok(())
}

async fn stale<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    environment: &EnvironmentId,
    session_id: &SessionId,
    producer: &ProducerId,
) -> Result<()> {
    let item = sylph_protocol::SequencedEvent::pending(
        producer.clone(),
        ProducerSeq::new(2),
        session_id.clone(),
        terminal_event(),
        None,
    );
    write_frame(
        writer,
        &WireMessage::from_event_batch(EventBatch::new(
            environment.clone(),
            producer.clone(),
            1,
            vec![item],
        )?),
    )
    .await?;
    match tokio::time::timeout(std::time::Duration::from_secs(2), read_frame(reader)).await {
        Ok(Err(_)) => Ok(()),
        Ok(Ok(bytes)) => Err(anyhow!("stale frame received {:?}", decode(&bytes)?)),
        Err(_) => Err(anyhow!("bridge did not close the stale-epoch connection")),
    }
}

async fn malformed<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    write_raw_frame(writer, br#"{"type":"event_batch","events":"not-an-array"}"#).await?;
    match tokio::time::timeout(std::time::Duration::from_secs(2), read_frame(reader)).await {
        Ok(Err(_)) => Ok(()),
        Ok(Ok(bytes)) => Err(anyhow!("malformed frame received {:?}", decode(&bytes)?)),
        Err(_) => Err(anyhow!(
            "bridge did not close the malformed-frame connection"
        )),
    }
}

fn terminal_event() -> Event {
    let mut event = Event::new(EventKind::Turn, Some(Phase::Completed), 42);
    event.text = "wire process complete".into();
    event.data = Some(serde_json::json!({"stopReason": "end_turn"}));
    event
}

async fn send_result<W: AsyncWrite + Unpin>(
    writer: &mut W,
    request_id: sylph_protocol::CommandRequestId,
    idempotency: IdempotencyPolicy,
    epoch: u64,
    outcome: CommandOutcome,
) -> Result<()> {
    write_frame(
        writer,
        &WireMessage::CommandResult {
            request_id,
            outcome,
            connection_epoch: epoch,
            idempotency: Some(idempotency),
        },
    )
    .await
}

fn command_name(command: &EnvironmentCommand) -> &'static str {
    match command {
        EnvironmentCommand::Prepare { .. } => "prepare",
        EnvironmentCommand::Run { .. } => "run",
        EnvironmentCommand::Send { .. } => "send",
        EnvironmentCommand::Interrupt { .. } => "interrupt",
        EnvironmentCommand::Steer { .. } => "steer",
        EnvironmentCommand::Queue { .. } => "queue",
        EnvironmentCommand::SetConfig { .. } => "set_config",
        EnvironmentCommand::Resolve { .. } => "resolve",
        EnvironmentCommand::Watch { .. } => "watch",
        EnvironmentCommand::Adopt { .. } => "adopt",
        EnvironmentCommand::Reread { .. } => "reread",
        EnvironmentCommand::Files { .. } => "files",
        EnvironmentCommand::DraftCapabilities { .. } => "draft_capabilities",
        EnvironmentCommand::DraftSetConfig { .. } => "draft_set_config",
        EnvironmentCommand::DraftFiles { .. } => "draft_files",
        EnvironmentCommand::DraftClose { .. } => "draft_close",
        EnvironmentCommand::Health => "health",
        EnvironmentCommand::Capabilities { .. } => "capabilities",
        EnvironmentCommand::Enumerate { .. } => "enumerate",
        EnvironmentCommand::Activate { .. } => "activate",
        EnvironmentCommand::Close { .. } => "close",
        EnvironmentCommand::BrowseRoots => "browse_roots",
        EnvironmentCommand::BrowseTree { .. } => "browse_tree",
        EnvironmentCommand::Reproject { .. } => "reproject",
        EnvironmentCommand::NativeTransfer { .. } => "native-transfer",
    }
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(anyhow!("invalid frame length {length}"));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

async fn write_json_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<()> {
    write_raw_frame(writer, &serde_json::to_vec(value)?).await
}

async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, value: &WireMessage) -> Result<()> {
    write_json_frame(writer, value).await
}

async fn write_raw_frame<W: AsyncWrite + Unpin>(writer: &mut W, payload: &[u8]) -> Result<()> {
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(anyhow!("invalid frame length {}", payload.len()));
    }
    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

fn decode(payload: &[u8]) -> Result<DecodedMessage> {
    Ok(sylph_protocol::decode_message(payload)?)
}
