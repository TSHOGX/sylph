//! Explicit JSON indexing owns a view independently of ordinary reader requests.
use super::*;
use crate::trace::{
    structure::{index, parser::Span, Job, State as JobState},
};
use std::{
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Create {
    via_ref: String,
    search_result: Option<String>,
    at_snapshot: String,
    revision: String,
    representation: Representation,
    selection: Option<SelectionRef>,
    duration_ms: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueryPage {
    node: Option<u64>,
    after: Option<u64>,
}

fn failure(status: StatusCode, code: &'static str, message: impl Into<String>) -> TraceError {
    TraceError {
        status,
        code,
        message: message.into(),
        response: None,
    }
}

async fn authorized(state: &AppState, job: &Job) -> Result<()> {
    if job.cancel.load(Ordering::Relaxed) || Instant::now() >= job.deadline {
        return Err(failure(
            StatusCode::GONE,
            "structure_expired",
            "Structured exploration expired or was cancelled",
        ));
    }
    state
        .trace
        .catalogue()
        .resolve_lease(&job.subject, &job.lease.token, crate::models::now_ms())
        .await
        .map_err(|error| lease_error(error.into()))?;
    check_row(state, &job.subject, &job.lease, &job.row).await?;
    check_epochs(state, &job.subject, &job.lease, &job.dependencies).await
}

pub(super) async fn create(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<Create>,
) -> Result<Response> {
    let payload = PayloadRef::parse(encoded).map_err(|error| invalid(error.to_string()))?;
    let original = lease(&state, &subject.0, Some(&query.at_snapshot)).await?;
    let grant = payload_access::authorize(
        &state,
        &subject.0,
        &original,
        &query.via_ref,
        query.selection,
        payload.clone(),
        query.revision,
        query.representation,
        query.search_result.as_deref(),
    )
    .await?;
    let total = state
        .trace
        .evidence()
        .payload_length(&payload)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            failure(
                StatusCode::CONFLICT,
                "structure_unavailable",
                "Payload length is unknown",
            )
        })?;
    let limits = state.trace.structures.limits.clone();
    let duration_ms = query.duration_ms.unwrap_or(limits.lifetime_ms);
    if duration_ms == 0 || duration_ms > limits.lifetime_ms {
        return Err(invalid(
            "Structured job duration exceeds its lifetime bound",
        ));
    }

    if total > limits.max_bytes {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "structure_limit",
            format!(
                "Structured exploration is limited to {} source bytes",
                limits.max_bytes
            ),
        ));
    }
    grant.range(0, total)?;
    let permit = state.trace.structures.reserve().ok_or_else(|| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "structure_capacity",
            "Close another structured exploration to release capacity",
        )
    })?;
    let owned = state
        .trace
        .transfer_lease(&subject.0, &original.token, duration_ms as i64)
        .await
        .map_err(lease_error)?;
    grant.check(&state, &subject.0, &original).await?;
    check_row(&state, &subject.0, &owned, &grant.row).await?;
    check_epochs(&state, &subject.0, &owned, &grant.dependencies).await?;
    let expires_at_ms = owned.expires_at_ms;
    let remaining_ms = expires_at_ms.saturating_sub(crate::models::now_ms()).max(0) as u64;
    let deadline = Instant::now() + Duration::from_millis(remaining_ms);
    let job = Arc::new(Job {
        id: uuid::Uuid::new_v4().to_string(),
        subject: subject.0,
        lease: owned,
        row: grant.row,
        anchor: grant.anchor,
        dependencies: grant.dependencies,
        total,
        expires_at_ms,
        deadline,
        cancel: Arc::new(AtomicBool::new(false)),
        bytes: AtomicU64::new(0),
        nodes: Arc::new(AtomicU64::new(0)),
        state: tokio::sync::RwLock::new(JobState::Building),
        _permit: permit,
    });
    let response = status(&state, &job, StructureState::Building, None);
    state.trace.structures.insert(job.clone()).await;
    tokio::spawn(async move {
        let source = Source {
            state: state.clone(),
            job: job.clone(),
            handle: tokio::runtime::Handle::current(),
            offset: 0,
            buffer: vec![],
            used: 0,
        };
        let built = match tempfile::Builder::new()
            .prefix("job-")
            .tempdir_in(&state.trace.structures.directory)
        {
            Ok(directory) => {
                index::Index::build(
                    directory,
                    source,
                    limits,
                    job.cancel.clone(),
                    job.nodes.clone(),
                    job.deadline,
                )
                .await
            }
            Err(error) => Err(error.into()),
        };
        let result = match built {
            Ok(index) => match authorized(&state, &job).await {
                Ok(()) => JobState::Ready(Arc::new(index)),
                Err(error) => JobState::Failed(error.message),
            },
            Err(error) => JobState::Failed(error.to_string()),
        };
        *job.state.write().await = result;
    });
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

fn status(
    state: &AppState,
    job: &Job,
    kind: StructureState,
    error: Option<&str>,
) -> StructureStatus {
    StructureStatus {
        id: job.id.clone(),
        view: read_view(&job.lease),
        state: kind,
        error: error.map(str::to_owned),
        bytes_read: job.bytes.load(Ordering::Relaxed),
        total_bytes: job.total,
        nodes: job.nodes.load(Ordering::Relaxed),
        expires_at_ms: job.expires_at_ms,
        limits: state.trace.structures.limits.clone(),
        node: None,
        items: None,
        after: None,
    }
}

pub(super) async fn read(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
    Query(query): Query<QueryPage>,
) -> Result<Json<StructureStatus>> {
    let job = state
        .trace
        .structures
        .get(&id, &subject.0)
        .await
        .ok_or_else(|| {
            failure(
                StatusCode::NOT_FOUND,
                "structure_unavailable",
                "Structured exploration is absent or expired",
            )
        })?;
    authorized(&state, &job).await?;
    let (index, error) = match &*job.state.read().await {
        JobState::Building => {
            return Ok(Json(status(&state, &job, StructureState::Building, None)));
        }
        JobState::Ready(index) => (Some(index.clone()), None),
        JobState::Failed(error) => (None, Some(error.clone())),
    };
    let Some(index) = index else {
        return Ok(Json(status(
            &state,
            &job,
            StructureState::Failed,
            error.as_deref(),
        )));
    };
    if query
        .node
        .is_some_and(|node| node == 0 || node > state.trace.structures.limits.max_nodes)
        || query
            .after
            .is_some_and(|ordinal| ordinal >= state.trace.structures.limits.max_nodes)
    {
        return Err(invalid("Structural position exceeds its index bounds"));
    }
    let node = index
        .node(query.node.unwrap_or(1))
        .await
        .map_err(internal)?
        .ok_or_else(|| invalid("Unknown structural node"))?;
    let limit = state.trace.structures.limits.page_items;
    let mut children = index
        .children(node.id, query.after, limit + 1)
        .await
        .map_err(internal)?;
    let more = children.len() > limit;
    children.truncate(limit);
    let after = more.then(|| children.last().unwrap().ordinal);
    let mut items = Vec::new();
    for child in children {
        items.push(preview(&state, &job, &child).await?);
    }
    let mut response = status(&state, &job, StructureState::Ready, None);
    response.node = Some(preview(&state, &job, &node).await?);
    response.items = Some(items);
    response.after = after;
    #[cfg(test)]
    if let Some((entered, release)) = state.trace.structure_page_pause.lock().await.take() {
        entered.notify_one();
        release.notified().await;
    }
    authorized(&state, &job).await?;
    Ok(Json(response))
}

async fn preview(state: &AppState, job: &Job, node: &index::Node) -> Result<StructureNode> {
    Ok(StructureNode {
        id: node.id,
        parent: node.parent,
        ordinal: node.ordinal,
        kind: node.kind,
        start: node.value.start,
        end: node.value.end,
        children: node.children,
        preview: if node.children == 0 {
            Some(text_preview(state, job, node.value).await?)
        } else {
            None
        },
        name: match node.name {
            Some(span) => Some(text_preview(state, job, span).await?),
            None => None,
        },
    })
}
async fn text_preview(state: &AppState, job: &Job, span: Span) -> Result<StructurePreview> {
    authorized(state, job).await?;
    let maximum = state.trace.structures.limits.preview_bytes;
    let size = (span.end - span.start).min(maximum as u64) as usize;
    let body = state
        .trace
        .evidence()
        .payload_range(&job.anchor.payload_ref, span.start, size)
        .await
        .map_err(internal)?
        .bytes;
    let text = match std::str::from_utf8(&body) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&body[..error.valid_up_to()]).map_err(internal)?
        }
        Err(error) => return Err(internal(error)),
    };
    Ok(StructurePreview {
        text: text.into(),
        truncated: (text.len() as u64) < span.end - span.start,
        start: span.start,
        end: span.start + text.len() as u64,
    })
}

pub(super) async fn remove(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.trace.structures.remove(&id, &subject.0).await;
    Ok(StatusCode::NO_CONTENT)
}

struct Source {
    state: AppState,
    job: Arc<Job>,
    handle: tokio::runtime::Handle,
    offset: u64,
    buffer: Vec<u8>,
    used: usize,
}
impl Read for Source {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.job.cancel.load(Ordering::Relaxed) || Instant::now() >= self.job.deadline {
            return Err(io::Error::other(
                "Structured exploration cancelled or expired",
            ));
        }
        if self.used == self.buffer.len() {
            #[cfg(test)]
            self.handle.block_on(async {
                if let Some((entered, release)) =
                    self.state.trace.structure_scan_pause.lock().await.take()
                {
                    entered.notify_one();
                    release.notified().await;
                }
            });
            self.handle
                .block_on(authorized(&self.state, &self.job))
                .map_err(|error| io::Error::other(error.message))?;
            if self.offset == self.job.total {
                return Ok(0);
            }
            let size = (self.job.total - self.offset)
                .min(self.state.trace.structures.limits.chunk_bytes as u64)
                as usize;
            self.buffer = self
                .handle
                .block_on(self.state.trace.evidence().payload_range(
                    &self.job.anchor.payload_ref,
                    self.offset,
                    size,
                ))
                .map_err(io::Error::other)?
                .bytes;
            if self.buffer.len() != size {
                return Err(io::Error::other("Retained JSON range was incomplete"));
            }
            self.offset += size as u64;
            self.used = 0;
            self.job.bytes.store(self.offset, Ordering::Relaxed);
        }
        let length = output.len().min(self.buffer.len() - self.used);
        output[..length].copy_from_slice(&self.buffer[self.used..self.used + length]);
        self.used += length;
        Ok(length)
    }
}
