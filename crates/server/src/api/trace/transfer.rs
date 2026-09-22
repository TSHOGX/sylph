//! Explicit one-payload jobs retain their own snapshot and bounded output queue.
use super::*;
use crate::trace::{generation::PayloadAnchor, service::LeaseGuard};
use axum::{
    body::{Body, Bytes},
    http::HeaderValue,
};
use std::{
    collections::BTreeSet,
    io::{self, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum TransferKind {
    Copy,
    Download,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TransferQuery {
    via_ref: String,
    at_snapshot: String,
    revision: String,
    representation: Representation,
    selection: Option<SelectionRef>,
    kind: TransferKind,
    #[serde(default)]
    buffered: bool,
    duration_ms: Option<u64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    payload: PayloadRef,
    body_revision: String,
    representation: Representation,
    total_bytes: Option<u64>,
    observed_bytes: u64,
    ranges: Vec<PayloadRange>,
    coverage: PayloadCompleteness,
}
struct Job {
    state: AppState,
    subject: String,
    lease: LeaseGuard,
    row: ReadRow,
    anchor: PayloadAnchor,
    dependencies: BTreeSet<IngestionEpoch>,
    manifest: Manifest,
}
fn failure(status: StatusCode, code: &'static str, message: impl Into<String>) -> TraceError {
    TraceError {
        status,
        code,
        message: message.into(),
        response: None,
    }
}
fn stream_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn normalize(ranges: Vec<PayloadRange>) -> Result<Vec<PayloadRange>> {
    let mut joined: Vec<PayloadRange> = vec![];
    for range in ranges {
        if range.start > range.end_exclusive {
            return Err(internal("invalid retained range"));
        }
        if let Some(previous) = joined.last_mut() {
            if range.start < previous.end_exclusive {
                return Err(internal("overlapping retained ranges"));
            }
            if range.start == previous.end_exclusive {
                previous.end_exclusive = range.end_exclusive;
                continue;
            }
        }
        joined.push(range);
    }
    Ok(joined)
}
impl Job {
    async fn authorized(&self) -> Result<()> {
        self.state
            .trace
            .catalogue()
            .resolve_lease(&self.subject, &self.lease.token, crate::models::now_ms())
            .await
            .map_err(|error| lease_error(error.into()))?;
        check_row(&self.state, &self.subject, &self.lease, &self.row).await?;
        check_epochs(&self.state, &self.subject, &self.lease, &self.dependencies).await
    }
    async fn chunk(&self, offset: u64, length: usize) -> io::Result<Vec<u8>> {
        self.authorized()
            .await
            .map_err(|error| stream_error(error.message))?;
        let bytes = self
            .state
            .trace
            .evidence()
            .payload_range(&self.anchor.payload_ref, offset, length)
            .await
            .map_err(stream_error)?
            .bytes;
        if bytes.len() != length {
            return Err(stream_error("retained payload ended early"));
        }
        self.authorized()
            .await
            .map_err(|error| stream_error(error.message))?;
        Ok(bytes)
    }
}

// zip's streaming writer needs only Write. Stored entries make CRC/header work
// bounded; this sink never performs I/O or seeks back over emitted bytes.
#[derive(Clone)]
struct Buffer {
    bytes: Arc<Mutex<Vec<u8>>>,
    maximum: usize,
}
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut buffer = self.bytes.lock().unwrap();
        if buffer.len().saturating_add(bytes.len()) > self.maximum {
            return Err(stream_error("ZIP output chunk exceeds its budget"));
        }
        buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Buffer {
    fn take(&self) -> Bytes {
        Bytes::from(std::mem::take(&mut *self.bytes.lock().unwrap()))
    }
}
async fn emit(
    job: &Job,
    sender: &mpsc::Sender<Bytes>,
    bytes: Bytes,
    total: &mut u64,
    maximum: u64,
) -> io::Result<()> {
    *total = total
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| stream_error("transfer size overflow"))?;
    if *total > maximum {
        return Err(stream_error("transfer output exceeds its byte budget"));
    }
    if bytes.is_empty() {
        return Ok(());
    }
    // Waiting for the receiver may outlive a grant. Reserve space before the
    // final authority check so backpressure cannot send a newly forbidden chunk.
    #[cfg(test)]
    if sender.capacity() == 0 {
        job.state.trace.transfer_backpressure.notify_one();
    }
    let slot = sender
        .reserve()
        .await
        .map_err(|_| stream_error("transfer cancelled"))?;
    job.authorized()
        .await
        .map_err(|error| stream_error(error.message))?;
    slot.send(bytes);
    Ok(())
}
async fn produce(
    job: &Job,
    kind: TransferKind,
    sender: &mpsc::Sender<Bytes>,
    maximum: u64,
) -> io::Result<()> {
    let limits = job.state.trace.transfer_limits();
    let chunk_bytes = limits
        .chunk_bytes
        .min(job.state.trace.limits().max_payload_bytes);
    let mut sent = 0;
    let buffer = Buffer {
        bytes: Arc::new(Mutex::new(Vec::new())),
        maximum: chunk_bytes + job.state.trace.limits().max_summary_bytes * 2,
    };
    let mut zip = (kind == TransferKind::Download).then(|| ZipWriter::new_stream(buffer.clone()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    if let Some(writer) = &mut zip {
        writer
            .start_file("manifest.json", options)
            .map_err(stream_error)?;
        writer.write_all(&serde_json::to_vec_pretty(&job.manifest).map_err(stream_error)?)?;
        emit(job, sender, buffer.take(), &mut sent, maximum).await?;
    }
    for range in &job.manifest.ranges {
        if let Some(writer) = &mut zip {
            writer
                .start_file(
                    format!("bytes/{}-{}.bin", range.start, range.end_exclusive),
                    options,
                )
                .map_err(stream_error)?;
        }
        let mut offset = range.start;
        while offset < range.end_exclusive {
            #[cfg(test)]
            {
                let pause = job.state.trace.transfer_pause.lock().await.take();
                if let Some((entered, release)) = pause {
                    entered.notify_one();
                    release.notified().await;
                }
            }
            let length = (range.end_exclusive - offset).min(chunk_bytes as u64) as usize;
            let bytes = job.chunk(offset, length).await?;
            let bytes = if let Some(writer) = &mut zip {
                writer.write_all(&bytes)?;
                buffer.take()
            } else {
                Bytes::from(bytes)
            };
            emit(job, sender, bytes, &mut sent, maximum).await?;
            offset += length as u64;
        }
    }
    if let Some(writer) = zip {
        writer.finish().map_err(stream_error)?;
        emit(job, sender, buffer.take(), &mut sent, maximum).await?;
    }
    Ok(())
}

pub(super) async fn payload_transfer(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    query: std::result::Result<Query<TransferQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response> {
    let Query(query) = query.map_err(|error| invalid(error.to_string()))?;
    let limits = state.trace.transfer_limits().clone();
    let duration_ms = query.duration_ms.unwrap_or(limits.lifetime_ms as u64);
    if duration_ms == 0 || duration_ms > limits.lifetime_ms as u64 {
        return Err(invalid(
            "Transfer duration must be positive and within the server limit",
        ));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(duration_ms);
    tokio::time::timeout_at(
        deadline,
        prepare_transfer(state, subject.0, encoded, query, deadline),
    )
    .await
    .map_err(|_| {
        failure(
            StatusCode::REQUEST_TIMEOUT,
            "transfer_timeout",
            "Transfer preparation exceeded its duration budget",
        )
    })?
}
async fn prepare_transfer(
    state: AppState,
    subject_id: String,
    encoded: String,
    query: TransferQuery,
    deadline: tokio::time::Instant,
) -> Result<Response> {
    let subject = SubjectId(subject_id);
    let limits = state.trace.transfer_limits().clone();
    let duration_ms = query.duration_ms.unwrap_or(limits.lifetime_ms as u64);
    let permit = state.trace.transfer_permit().ok_or_else(|| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "transfer_capacity",
            "Another observed-content transfer is using the available capacity",
        )
    })?;
    let payload = PayloadRef::parse(encoded).map_err(|error| invalid(error.to_string()))?;
    let reference = reference(&query.via_ref)?;
    observe(&state, &subject.0, reference.scope()).await?;
    // Resolve before copying: the original view cannot name another subject/cut.
    let original = lease(&state, &subject.0, Some(&query.at_snapshot)).await?;
    let generation = state
        .trace
        .generation(&original.snapshot.generation)
        .await
        .map_err(internal)?;
    let row = generation
        .read_row(
            &original.snapshot,
            &read_scope(reference.scope(), None, "objects"),
            &reference.encode(),
        )
        .await
        .map_err(storage)?;
    check_row(&state, &subject.0, &original, &row).await?;
    let detail = generation
        .object(&original.snapshot, &reference)
        .await
        .map_err(storage)?;
    let owner = crate::trace::generation::payload_anchor_owner(&detail.object)
        .cloned()
        .ok_or_else(|| invalid("Payload authorization needs a readable containing object"))?;
    observe(&state, &subject.0, &owner).await?;
    let anchor = PayloadAnchor {
        via_ref: reference,
        containing_agent: owner,
        selection: query.selection,
        payload_ref: payload.clone(),
        body_revision: query.revision.clone(),
        representation: query.representation,
        ranges: vec![],
        dependency_epochs: Default::default(),
    };
    let (ranges, dependencies) = generation
        .payload_ranges(&original.snapshot, &anchor)
        .await
        .map_err(storage)?;
    check_epochs(&state, &subject.0, &original, &dependencies).await?;
    let mut ranges = normalize(ranges)?;
    let original_length = state
        .trace
        .evidence()
        .payload_length(&payload)
        .await
        .map_err(internal)?;
    if ranges.is_empty() {
        if original_length == Some(0) {
            ranges.push(PayloadRange {
                start: 0,
                end_exclusive: 0,
            });
        } else {
            return Err(failure(
                StatusCode::CONFLICT,
                "transfer_unavailable",
                "No complete retained interval is available to transfer",
            ));
        }
    }
    if query.kind == TransferKind::Copy && ranges.len() != 1 {
        return Err(failure(
            StatusCode::CONFLICT,
            "transfer_gap",
            "Copy cannot join missing intervals; download the observed-content archive instead",
        ));
    }
    let observed_bytes = ranges
        .iter()
        .try_fold(0u64, |sum, range| {
            sum.checked_add(range.end_exclusive - range.start)
        })
        .ok_or_else(|| invalid("Transfer size overflow"))?;
    let source_limit = if query.kind == TransferKind::Copy {
        limits.max_copy_bytes
    } else {
        limits.max_download_bytes
    };
    if observed_bytes > source_limit {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "transfer_limit",
            format!("Observed content exceeds the transfer limit of {source_limit} bytes"),
        ));
    }
    let mut coverage = state
        .trace
        .evidence()
        .payload_manifest(&payload)
        .await
        .map_err(internal)?
        .map_or(PayloadCompleteness::Complete, |manifest| {
            manifest.completeness
        });
    if coverage == PayloadCompleteness::Complete
        && (ranges.len() != 1
            || ranges[0].start != 0
            || Some(ranges[0].end_exclusive) != original_length)
    {
        coverage = PayloadCompleteness::KnownRanges {
            ranges: ranges.clone(),
        };
    }
    let manifest = Manifest {
        payload,
        body_revision: query.revision,
        representation: query.representation,
        total_bytes: original_length,
        observed_bytes,
        ranges,
        coverage,
    };
    let metadata_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(internal)?
        .len();
    if metadata_bytes > state.trace.limits().max_summary_bytes {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "transfer_limit",
            "Observed range metadata exceeds the transfer budget",
        ));
    }
    // Stored ZIP headers/central entries are bounded by these short numeric names.
    let output_bound = observed_bytes
        .checked_add(metadata_bytes as u64)
        .and_then(|size| size.checked_add((manifest.ranges.len() as u64 + 1) * 512 + 1024))
        .ok_or_else(|| invalid("Transfer size overflow"))?;
    if query.buffered && output_bound > limits.max_buffered_bytes {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "transfer_limit",
            format!(
                "This browser's buffered download exceeds {} bytes; use a browser with a streaming file writer",
                limits.max_buffered_bytes
            ),
        ));
    }
    let output_limit = if query.kind == TransferKind::Copy {
        limits.max_copy_bytes
    } else if query.buffered {
        limits.max_buffered_bytes
    } else {
        output_bound
    };
    let owned = state
        .trace
        .transfer_lease(&subject.0, &original.token, duration_ms as i64)
        .await
        .map_err(lease_error)?;
    let job = Job {
        state,
        subject: subject.0,
        lease: owned,
        row,
        anchor,
        dependencies,
        manifest,
    };
    job.authorized().await?;
    let (sender, mut receiver) = mpsc::channel::<Bytes>(2);
    let terminal = Arc::new(Mutex::new(None::<io::Result<()>>));
    let finish = terminal.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let result = tokio::select! {
            _ = sender.closed() => Err(stream_error("transfer cancelled")),
            result = tokio::time::timeout_at(deadline,produce(&job,query.kind,&sender,output_limit)) => result.unwrap_or_else(|_|Err(stream_error("transfer exceeded its duration budget"))),
        };
        *finish.lock().unwrap() = Some(result);
        // The owned lease guard and permit are dropped on every exit, including
        // disconnect and a receiver stalled behind the bounded channel.
    });
    let stream = async_stream::stream! {
        while let Some(bytes) = receiver.recv().await {
            let error = terminal.lock().unwrap().as_ref().and_then(|result| result.as_ref().err()).map(stream_error);
            if let Some(error) = error { yield Err::<Bytes,io::Error>(error); return; }
            yield Ok(bytes);
        }
        let result = terminal.lock().unwrap().take();
        match result {
            Some(Ok(())) => {}, Some(Err(error)) => yield Err(error), None => yield Err(stream_error("transfer ended without completion")),
        }
    };
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store, no-transform"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(if query.kind == TransferKind::Copy {
            "application/octet-stream"
        } else {
            "application/zip"
        }),
    );
    response.headers_mut().insert(
        "x-sylph-transfer-duration-ms",
        HeaderValue::from_str(&duration_ms.to_string()).map_err(internal)?,
    );
    response.headers_mut().insert(
        "x-sylph-transfer-limit",
        HeaderValue::from_str(&output_limit.to_string()).map_err(internal)?,
    );
    response.headers_mut().insert(
        "x-sylph-observed-bytes",
        HeaderValue::from_str(&observed_bytes.to_string()).map_err(internal)?,
    );
    if query.kind == TransferKind::Copy {
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&observed_bytes.to_string()).map_err(internal)?,
        );
    }
    Ok(response)
}
