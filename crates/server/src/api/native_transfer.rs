//! Native passes cross the envagent boundary in bounded chunks and validate before ingest.

use std::io::{BufRead, Write};

use anyhow::{bail, ensure, Context, Result};
use sylph_protocol::{
    CommandOutcome, EnvironmentCommand, EventStream, IdempotencyPolicy, NativeTransfer,
    NativeTransferChunk, NativeTransferItem, NativeTransferOperation, SessionId, SessionMessage,
    NATIVE_TRANSFER_CHUNK_BYTES,
};

use crate::AppState;

pub(super) async fn command(
    state: &AppState,
    session: &SessionId,
    command: EnvironmentCommand,
) -> Result<serde_json::Value> {
    match state
        .env_bridge
        .remote_session_command(session, command, IdempotencyPolicy::AtMostOnce)
        .await?
    {
        CommandOutcome::Accepted { result } => result.context("native transfer returned no result"),
        CommandOutcome::Rejected { code, message } => bail!("{code}: {message}"),
        CommandOutcome::Unknown { message } => bail!(
            "native transfer outcome is unknown: {}",
            message.unwrap_or_default()
        ),
    }
}

async fn operation(
    state: &AppState,
    session: &SessionId,
    operation: NativeTransferOperation,
) -> Result<serde_json::Value> {
    command(
        state,
        session,
        EnvironmentCommand::NativeTransfer {
            session_id: session.clone(),
            operation,
        },
    )
    .await
}

pub(super) async fn release(state: &AppState, session: &SessionId, id: String) {
    if let Err(error) = operation(state, session, NativeTransferOperation::Release { id }).await {
        tracing::debug!("native transfer cleanup: {error:#}");
    }
}

pub(super) struct DownloadedPass {
    file: tempfile::NamedTempFile,
    pub read_errors: usize,
}

impl DownloadedPass {
    pub fn stream(self) -> EventStream {
        Box::pin(async_stream::try_stream! {
            let reader = std::io::BufReader::new(self.file.reopen()?);
            for line in reader.lines() {
                let item: NativeTransferItem = serde_json::from_str(&line?)?;
                match item {
                    NativeTransferItem::Event(event) => yield SessionMessage::Event(event),
                    NativeTransferItem::Record(record) => yield SessionMessage::NativeRecord(record),
                    NativeTransferItem::TurnInput(input) => yield SessionMessage::TurnInput(input),
                    NativeTransferItem::Summary { .. } => {}
                }
            }
            drop(self.file);
        })
    }
}

pub(super) async fn download(
    state: &AppState,
    session: &SessionId,
    transfer: NativeTransfer,
) -> Result<DownloadedPass> {
    let result = async {
        let mut file = tempfile::NamedTempFile::new()?;
        let mut offset = 0;
        while offset < transfer.bytes {
            let chunk: NativeTransferChunk = serde_json::from_value(
                operation(
                    state,
                    session,
                    NativeTransferOperation::Read {
                        id: transfer.id.clone(),
                        offset,
                    },
                )
                .await?,
            )?;
            ensure!(
                !chunk.bytes.is_empty() && chunk.bytes.len() <= NATIVE_TRANSFER_CHUNK_BYTES,
                "native transfer returned an invalid chunk size"
            );
            ensure!(
                chunk.next_offset == offset + chunk.bytes.len() as u64
                    && chunk.next_offset <= transfer.bytes,
                "native transfer offset mismatch"
            );
            file.write_all(&chunk.bytes)?;
            offset = chunk.next_offset;
        }
        file.as_file().sync_all()?;
        // Parsing the entire file before ingest makes malformed/truncated downloads an
        // operation failure. Memory is bounded by one record, not the session's history.
        let mut summary = None;
        for line in std::io::BufReader::new(file.reopen()?).lines() {
            ensure!(
                summary.is_none(),
                "native transfer has data after its summary"
            );
            if let NativeTransferItem::Summary { read_errors, .. } = serde_json::from_str(&line?)? {
                summary = Some(read_errors);
            }
        }
        Ok(DownloadedPass {
            file,
            read_errors: summary.context("native transfer is incomplete")?,
        })
    }
    .await;
    release(state, session, transfer.id).await;
    result
}
