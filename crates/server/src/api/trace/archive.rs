//! Lossless evidence frames. No vendor file reads or current-value transcript reconstruction.
use super::*;
use crate::trace::{
    evidence::{
        export::ExportTicket,
        catalogue::{PayloadPin, PinOwnerKind},
    },
    service::LeaseGuard,
    evidence_export::{Limits, Pacing},
};
use axum::body::{Body, Bytes};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{
    io,
    sync::{Arc, Mutex},
    collections::BTreeSet,
    time::Duration,
};
use tokio::sync::mpsc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueryExport {
    at_snapshot: String,
    buffered: Option<bool>,
    duration_ms: Option<u64>,
    max_bytes: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueryPlayback {
    at_snapshot: String,
    duration_ms: Option<u64>,
    max_bytes: Option<u64>,
    speed: Option<u8>,
    max_gap_ms: Option<u64>,
}
struct Options {
    query: QueryExport,
    pacing: Option<Pacing>,
    limits: Limits,
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
struct Job {
    state: AppState,
    subject: String,
    lease: LeaseGuard,
    ticket: ExportTicket,
    maximum: u64,
    limits: Limits,
    pacing: Option<Pacing>,
    manifest: crate::trace::generation::GenerationManifest,
}
impl Job {
    async fn authorized(&self) -> Result<()> {
        self.state
            .trace
            .catalogue()
            .resolve_lease(&self.subject, &self.lease.token, crate::models::now_ms())
            .await
            .map_err(|error| lease_error(error.into()))?;
        if !self
            .state
            .store
            .has_capability(&self.subject, self.ticket.environment.as_str(), "observe")
            .await
            .map_err(internal)?
        {
            return Err(failure(
                StatusCode::FORBIDDEN,
                "export_forbidden",
                "Environment evidence access was revoked",
            ));
        }
        if !self
            .state
            .trace
            .evidence()
            .export_is_current(&self.ticket)
            .await
            .map_err(internal)?
        {
            return Err(failure(
                StatusCode::GONE,
                "export_invalidated",
                "Evidence export expired or the environment's retained visibility changed",
            ));
        }
        Ok(())
    }
    async fn release(&self) {
        #[cfg(test)]
        if let Some((entered, release)) = self
            .state
            .trace
            .evidence_export_cleanup_pause
            .lock()
            .await
            .take()
        {
            entered.notify_one();
            release.notified().await;
        }
        let catalogue = self.state.trace.catalogue();
        let maximum = catalogue.limits().max_pin_batch;
        loop {
            match catalogue
                .release_owner_pins(PinOwnerKind::Export, &self.ticket.id, maximum)
                .await
            {
                Ok(count) if count == maximum => continue,
                Ok(_) => {
                    let _ = self
                        .state
                        .trace
                        .evidence()
                        .finish_export(&self.ticket.id)
                        .await;
                    break;
                }
                Err(error) => {
                    tracing::error!(%error,"Evidence export cleanup deferred to expiry recovery");
                    break;
                }
            }
        }
    }
}
struct Output<'a> {
    job: &'a Job,
    sender: &'a mpsc::Sender<Bytes>,
    bytes: u64,
    frames: u64,
    hash: Sha256,
}
impl Output<'_> {
    async fn frame(&mut self, value: serde_json::Value, hashed: bool) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(&value).map_err(stream_error)?;
        bytes.push(b'\n');
        if bytes.len() > self.job.limits.frame_bytes {
            return Err(stream_error("Evidence frame exceeds its metadata limit"));
        }
        let next = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| stream_error("Export byte overflow"))?;
        if next > self.job.maximum {
            return Err(stream_error("Evidence export exceeded its output budget"));
        }
        #[cfg(test)]
        if self.sender.capacity() == 0 {
            self.job
                .state
                .trace
                .evidence_export_backpressure
                .notify_one();
        }
        let slot = self
            .sender
            .reserve()
            .await
            .map_err(|_| stream_error("Evidence export cancelled"))?;
        self.job
            .authorized()
            .await
            .map_err(|error| stream_error(error.message))?;
        if hashed {
            self.hash.update(&bytes);
            self.frames += 1;
        }
        self.bytes = next;
        slot.send(Bytes::from(bytes));
        Ok(())
    }
}
async fn payload(
    output: &mut Output<'_>,
    capture: &crate::trace::evidence::export::ExportCapture,
    reference: &PayloadRef,
    source_bytes: &mut u64,
) -> io::Result<()> {
    let job = output.job;
    job.authorized()
        .await
        .map_err(|error| stream_error(error.message))?;
    job.state
        .trace
        .catalogue()
        .pin_export_payload(
            &job.ticket,
            PayloadPin {
                payload: reference.clone(),
                epoch: capture.capture.ingestion_authorization_epoch.clone(),
            },
        )
        .await
        .map_err(stream_error)?;
    let manifest = job
        .state
        .trace
        .evidence()
        .payload_manifest(reference)
        .await
        .map_err(stream_error)?;
    let length = job
        .state
        .trace
        .evidence()
        .payload_length(reference)
        .await
        .map_err(stream_error)?;
    let mut ranges: Vec<PayloadRange> = match &manifest {
        None => vec![PayloadRange {
            start: 0,
            end_exclusive: length
                .ok_or_else(|| stream_error("Raw payload length is unavailable"))?,
        }],
        Some(manifest) => manifest
            .chunks
            .iter()
            .map(|chunk| PayloadRange {
                start: chunk.payload_offset,
                end_exclusive: chunk.payload_offset + chunk.length,
            })
            .collect(),
    };
    ranges.sort_by_key(|range| range.start);
    let mut joined: Vec<PayloadRange> = vec![];
    for range in ranges {
        if let Some(last) = joined.last_mut() {
            if last.end_exclusive == range.start {
                last.end_exclusive = range.end_exclusive;
                continue;
            }
            if last.end_exclusive > range.start {
                return Err(stream_error("Overlapping retained ranges"));
            }
        }
        joined.push(range);
    }
    let size = joined
        .iter()
        .try_fold(0u64, |sum, range| {
            sum.checked_add(range.end_exclusive - range.start)
        })
        .ok_or_else(|| stream_error("Export source size overflow"))?;
    *source_bytes = source_bytes
        .checked_add(size)
        .ok_or_else(|| stream_error("Export source size overflow"))?;
    if *source_bytes > job.limits.max_source_bytes {
        return Err(stream_error(
            "Evidence export exceeded its source-byte budget",
        ));
    }
    output.frame(serde_json::json!({"kind":"payload","observation":capture.capture.observation_ref,"payload":reference,"totalBytes":length,"manifest":manifest,"ranges":joined}),true).await?;
    for range in joined {
        let mut offset = range.start;
        while offset < range.end_exclusive {
            job.authorized()
                .await
                .map_err(|error| stream_error(error.message))?;
            let size = (range.end_exclusive - offset).min(job.limits.chunk_bytes as u64) as usize;
            let bytes = job
                .state
                .trace
                .evidence()
                .payload_range(reference, offset, size)
                .await
                .map_err(stream_error)?
                .bytes;
            #[cfg(test)]
            if let Some((entered, release)) = job
                .state
                .trace
                .evidence_export_payload_pause
                .lock()
                .await
                .take()
            {
                entered.notify_one();
                release.notified().await;
            }
            if bytes.len() != size {
                return Err(stream_error("Retained export range ended early"));
            }
            output.frame(serde_json::json!({"kind":"bytes","payload":reference,"offset":offset,"data":STANDARD.encode(&bytes)}),true).await?;
            offset += size as u64;
        }
    }
    output
        .frame(
            serde_json::json!({"kind":"payload_end","payload":reference,"observedBytes":size}),
            true,
        )
        .await?;
    job.state
        .trace
        .catalogue()
        .release_owner_pins(
            PinOwnerKind::Export,
            &job.ticket.id,
            job.state.trace.catalogue().limits().max_pin_batch,
        )
        .await
        .map_err(stream_error)?;
    Ok(())
}
async fn produce(job: &Job, sender: &mpsc::Sender<Bytes>) -> io::Result<()> {
    let mut output = Output {
        job,
        sender,
        bytes: 0,
        frames: 0,
        hash: Sha256::new(),
    };
    output.frame(serde_json::json!({"kind":"header","format":"sylph-evidence-jsonl","scope":"environment","environment":job.ticket.environment,"evidenceCut":job.ticket.evidence_cut,"projectionManifest":job.manifest,"snapshot":job.lease.snapshot,"visibilityRevision":job.ticket.visibility_revision,"selection":"currently retained admitted evidence; excludes tombstoned evidence and unadmitted staging","limits":job.limits,"pacing":job.pacing}),true).await?;
    #[cfg(test)]
    if let Some((entered, release)) = job.state.trace.evidence_export_pause.lock().await.take() {
        entered.notify_one();
        release.notified().await;
    }
    let mut previous_receipt = None;
    let (mut after, mut candidates, mut observations, mut source_bytes) = (0, 0, 0u64, 0u64);
    loop {
        job.authorized()
            .await
            .map_err(|error| stream_error(error.message))?;
        let page = job
            .state
            .trace
            .evidence()
            .export_page(&job.ticket.environment, after, job.ticket.evidence_cut, 32)
            .await
            .map_err(stream_error)?;
        candidates += page.candidates;
        if candidates > job.limits.max_admissions {
            return Err(stream_error(
                "Evidence export exceeded its admission-work budget",
            ));
        }
        for capture in page.captures {
            if let Some(pacing) = job.pacing {
                let timing = pacing.timing(
                    capture.admission_seq,
                    previous_receipt,
                    capture.admitted_at_ms,
                );
                let mut frame = serde_json::to_value(timing).map_err(stream_error)?;
                frame["kind"] = "timing".into();
                output.frame(frame, true).await?;
            }
            previous_receipt = Some(capture.admitted_at_ms);
            let mut frame = serde_json::json!({"kind":"capture","record":capture});
            if job.pacing.is_some() {
                // JavaScript numbers cannot preserve all i64 clock ticks or u64
                // producer sequences. Render metadata from these exact strings,
                // never from a parse/stringify round trip through browser numbers.
                frame["metadataText"] = serde_json::json!({
                    "observationRef": serde_json::to_string(&capture.capture.observation_ref).map_err(stream_error)?,
                    "sourcePosition": serde_json::to_string(&capture.capture.source_position).map_err(stream_error)?,
                    "capturedAt": serde_json::to_string(&capture.capture.captured_at).map_err(stream_error)?,
                    "sourceClock": serde_json::to_string(&capture.capture.source_clock).map_err(stream_error)?,
                });
            }
            output.frame(frame, true).await?;
            let payloads: BTreeSet<_> = std::iter::once(&capture.capture.payload_ref)
                .chain(
                    capture
                        .capture
                        .attachments
                        .iter()
                        .filter_map(|attachment| attachment.payload_ref.as_ref()),
                )
                .cloned()
                .collect();
            for reference in payloads {
                payload(&mut output, &capture, &reference, &mut source_bytes).await?;
            }
            observations += 1;
        }
        after = page.after;
        if !page.more {
            break;
        }
    }
    let footer = serde_json::json!({"kind":"complete","observations":observations,"sourceBytes":source_bytes,"frames":output.frames,"prefixBytes":output.bytes,"sha256":hex::encode(output.hash.clone().finalize())});
    output.frame(footer, false).await
}
pub(super) async fn export(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(environment): Path<String>,
    Query(query): Query<QueryExport>,
) -> Result<Response> {
    let limits = state.trace.evidence_export_limits.clone();
    start(
        state,
        subject,
        environment,
        Options {
            query,
            pacing: None,
            limits,
        },
    )
    .await
}

pub(super) async fn playback(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(environment): Path<String>,
    Query(query): Query<QueryPlayback>,
) -> Result<Response> {
    let speed = query.speed.unwrap_or(1);
    let max_gap_ms = query.max_gap_ms.unwrap_or(2000);
    if ![1, 2, 4, 10].contains(&speed) || max_gap_ms == 0 || max_gap_ms > 2000 {
        return Err(invalid(
            "Playback speed or gap exceeds its supported bounds",
        ));
    }
    let limits = state.trace.evidence_playback_limits.clone();
    let options = Options {
        query: QueryExport {
            at_snapshot: query.at_snapshot,
            buffered: Some(false),
            duration_ms: query.duration_ms,
            max_bytes: query.max_bytes,
        },
        pacing: Some(Pacing {
            clock: "server_admission",
            speed,
            max_gap_ms,
        }),
        limits,
    };
    start(state, subject, environment, options).await
}

async fn start(
    state: AppState,
    subject: SubjectId,
    environment: String,
    options: Options,
) -> Result<Response> {
    let duration = options
        .query
        .duration_ms
        .unwrap_or(options.limits.lifetime_ms as u64);
    if duration == 0 || duration > options.limits.lifetime_ms as u64 {
        return Err(invalid(
            "Evidence stream duration exceeds its configured lifetime",
        ));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(duration);
    tokio::time::timeout_at(
        deadline,
        prepare(state, subject, environment, options, deadline, duration),
    )
    .await
    .map_err(|_| {
        failure(
            StatusCode::REQUEST_TIMEOUT,
            "export_timeout",
            "Evidence export preparation exceeded its duration budget",
        )
    })?
}

async fn prepare(
    state: AppState,
    subject: SubjectId,
    environment: String,
    options: Options,
    deadline: tokio::time::Instant,
    duration: u64,
) -> Result<Response> {
    let Options {
        query,
        pacing,
        limits,
    } = options;
    let available = if pacing.is_some() {
        state.trace.evidence_playback_permit()
    } else {
        state.trace.evidence_export_permit()
    };
    let permit = available.ok_or_else(|| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "export_capacity",
            "Another evidence stream is using the available capacity",
        )
    })?;
    #[cfg(test)]
    if let Some((entered, release)) = state
        .trace
        .evidence_export_prepare_pause
        .lock()
        .await
        .take()
    {
        entered.notify_one();
        release.notified().await;
    }
    let environment = sylph_protocol::EnvironmentId::parse(environment)
        .map_err(|error| invalid(error.to_string()))?;
    if !state
        .store
        .has_capability(&subject.0, environment.as_str(), "observe")
        .await
        .map_err(internal)?
    {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "export_forbidden",
            "Environment evidence access is unavailable",
        ));
    }
    let capacity = if query.buffered.unwrap_or(false) {
        limits.max_buffered_bytes
    } else {
        limits.max_output_bytes
    };
    let maximum = query.max_bytes.unwrap_or(capacity);
    if maximum == 0 || maximum > capacity {
        return Err(invalid("Export output budget exceeds its configured bound"));
    }
    let original = lease(&state, &subject.0, Some(&query.at_snapshot)).await?;
    let owned = state
        .trace
        .evidence_export_lease(&subject.0, &original.token, duration as i64)
        .await
        .map_err(lease_error)?;
    let manifest = state
        .trace
        .generation(&owned.snapshot.generation)
        .await
        .map_err(internal)?
        .manifest()
        .clone();
    let ticket = state
        .trace
        .evidence()
        .begin_export(environment, owned.snapshot.evidence_cut, duration as i64)
        .await
        .map_err(internal)?;
    let job = Job {
        state,
        subject: subject.0,
        lease: owned,
        manifest,
        ticket,
        maximum,
        limits: limits.clone(),
        pacing,
    };
    if let Err(error) = job.authorized().await {
        job.release().await;
        return Err(error);
    }
    let (sender, mut receiver) = mpsc::channel::<Bytes>(2);
    let terminal = Arc::new(Mutex::new(None::<io::Result<()>>));
    let finish = terminal.clone();
    tokio::spawn(async move {
        let result = tokio::select! {_ = sender.closed()=>Err(stream_error("Evidence export disconnected")),result=tokio::time::timeout_at(deadline,produce(&job,&sender))=>result.unwrap_or_else(|_|Err(stream_error("Evidence export exceeded its duration budget")))};
        *finish.lock().unwrap() = Some(result);
        // Database retirement is separately recoverable. It cannot hold transport
        // EOF or active worker capacity after completion, timeout or disconnect.
        drop(sender);
        drop(permit);
        job.release().await;
    });
    let stream = async_stream::stream! {
        while let Some(bytes)=receiver.recv().await {
            let error=terminal.lock().unwrap().as_ref().and_then(|result|result.as_ref().err()).map(stream_error);
            if let Some(error)=error {yield Err::<Bytes,io::Error>(error);return}
            yield Ok(bytes);
        }
        let result=terminal.lock().unwrap().take();
        match result {Some(Ok(()))=>{},Some(Err(error))=>yield Err(error),None=>yield Err(stream_error("Export ended without completion"))}
    };
    let mut response = Body::from_stream(stream).into_response();
    for (name, value) in [
        ("content-type", "application/x-ndjson".to_owned()),
        ("cache-control", "private, no-store, no-transform".into()),
        ("x-sylph-export-limit", maximum.to_string()),
        ("x-sylph-export-frame-limit", limits.frame_bytes.to_string()),
        ("x-sylph-export-duration-ms", duration.to_string()),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).map_err(internal)?,
            axum::http::HeaderValue::from_str(&value).map_err(internal)?,
        );
    }
    Ok(response)
}
