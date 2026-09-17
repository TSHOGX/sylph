//! Authenticated normalized trace reads. Every request rechecks grants and tombstones.

use crate::{
    middleware::auth::SubjectId,
    trace::{
        evidence::catalogue::ViewLease,
        generation::GenerationError,
        reading_store::{read_scope, ReadCandidate, ReadFilter, ReadRow, ReadWindow},
    },
    AppState,
};
use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sylph_protocol::trace::*;

#[derive(Debug)]
pub struct TraceError {
    status: StatusCode,
    code: &'static str,
    message: String,
    response: Option<Box<Response>>,
}
impl IntoResponse for TraceError {
    fn into_response(self) -> Response {
        if let Some(response) = self.response {
            return *response;
        }
        (
            self.status,
            Json(serde_json::json!({"error":{"code":self.code,"message":self.message}})),
        )
            .into_response()
    }
}
type Result<T> = std::result::Result<T, TraceError>;

#[cfg(test)]
mod visibility_tests;
fn invalid(message: impl Into<String>) -> TraceError {
    TraceError {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_cursor",
        message: message.into(),
        response: None,
    }
}
fn internal(error: impl std::fmt::Display) -> TraceError {
    tracing::error!(%error,"trace request failed");
    TraceError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "trace_unavailable",
        message: "Trace storage is unavailable".into(),
        response: None,
    }
}
fn operational(error: crate::error::Error) -> TraceError {
    TraceError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal",
        message: String::new(),
        response: Some(Box::new(error.into_response())),
    }
}
fn storage(error: GenerationError) -> TraceError {
    match error {
        GenerationError::WorkBudget => TraceError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "trace_work_budget",
            message: error.to_string(),
            response: None,
        },
        GenerationError::PayloadNotReachable => TraceError {
            status: StatusCode::FORBIDDEN,
            code: "payload_not_reachable",
            message: error.to_string(),
            response: None,
        },
        GenerationError::InterpretationWithdrawn(_) => TraceError {
            status: StatusCode::GONE,
            code: "interpretation_withdrawn",
            message: error.to_string(),
            response: None,
        },
        GenerationError::SnapshotExpired => TraceError {
            status: StatusCode::GONE,
            code: "snapshot_expired",
            message: error.to_string(),
            response: None,
        },
        GenerationError::InvalidCursor | GenerationError::InvalidSnapshot => {
            invalid(error.to_string())
        }
        GenerationError::NotFound | GenerationError::AnchorOutsideSelection => TraceError {
            status: StatusCode::NOT_FOUND,
            code: "trace_object_not_found",
            message: error.to_string(),
            response: None,
        },
        _ => internal(error),
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/trace/agents/{agent}", get(bootstrap))
        .route("/api/v1/trace/agents/{agent}/history", get(history))
        .route("/api/v1/trace/agents/{agent}/relations", get(relations))
        .route("/api/v1/trace/agents/{agent}/activations", get(activations))
        .route("/api/v1/trace/agents/{agent}/histories", get(histories))
        .route("/api/v1/trace/agents/{agent}/control", get(control))
        .route("/api/v1/trace/agents/{agent}/commands", get(command_list))
        .route("/api/v1/trace/objects/{object}", get(object))
        .route(
            "/api/v1/trace/objects/{object}/relations",
            get(object_relations),
        )
        .route("/api/v1/trace/objects/{object}/parts", get(parts))
        .route("/api/v1/trace/payloads/{payload}", get(payload))
        .route("/api/v1/trace/commands", post(command))
        .route(
            "/api/v1/trace/commands/{id}",
            axum::routing::patch(crate::api::controls::edit).delete(crate::api::controls::cancel),
        )
        .route("/api/v1/trace/changes", get(changes))
        .route("/api/v1/trace/search", get(search))
        .route(
            "/api/v1/trace/reprojection",
            get(reprojection_status).post(start_reprojection),
        )
        .route(
            "/api/v1/trace/reprojection/{id}",
            get(reprojection_job).delete(cancel_reprojection),
        )
        .route(
            "/api/v1/trace/views/{token}",
            axum::routing::delete(release_view),
        )
}

async fn reprojection_status(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
) -> Result<Json<serde_json::Value>> {
    if !state.store.is_admin(&subject.0).await.map_err(internal)? {
        return Err(TraceError {
            status: StatusCode::FORBIDDEN,
            code: "admin_required",
            message: "Server administration authority required".into(),
            response: None,
        });
    }
    let pointer = state.trace.catalogue().pointer().await.map_err(internal)?;
    let jobs = state
        .trace
        .catalogue()
        .reprojection_jobs()
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({
        "activeGeneration": pointer.generation,
        "pointerSerial": pointer.serial,
        "jobs": jobs,
        "recovering": state.trace.progress().await.fault,
    })))
}

async fn start_reprojection(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    if !state.store.is_admin(&subject.0).await.map_err(internal)? {
        return Err(TraceError {
            status: StatusCode::FORBIDDEN,
            code: "admin_required",
            message: "Server administration authority required".into(),
            response: None,
        });
    }
    let job = state
        .trace
        .start_reprojection(&subject.0)
        .await
        .map_err(lease_error)?;
    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({"job":job}))))
}

async fn reprojection_job(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require_reprojection_admin(&state, &subject).await?;
    let job = state
        .trace
        .catalogue()
        .reprojection_job(&id)
        .await
        .map_err(internal)?
        .ok_or_else(reprojection_not_found)?;
    Ok(Json(serde_json::json!({"job":job})))
}

async fn cancel_reprojection(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    require_reprojection_admin(&state, &subject).await?;
    let job = state
        .trace
        .cancel_reprojection(&id)
        .await
        .map_err(internal)?
        .ok_or_else(reprojection_not_found)?;
    let status = if job.state == crate::trace::evidence::catalogue::ReprojectionState::Cancelling {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(serde_json::json!({"job":job}))))
}

async fn require_reprojection_admin(state: &AppState, subject: &SubjectId) -> Result<()> {
    if !state.store.is_admin(&subject.0).await.map_err(internal)? {
        return Err(TraceError {
            status: StatusCode::FORBIDDEN,
            code: "admin_required",
            message: "Server administration authority required".into(),
            response: None,
        });
    }
    Ok(())
}

fn reprojection_not_found() -> TraceError {
    TraceError {
        status: StatusCode::NOT_FOUND,
        code: "reprojection_not_found",
        message: "Reprojection job not found".into(),
        response: None,
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    agent: Option<String>,
    selection: Option<String>,
    at_snapshot: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}
async fn search(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ReadPage<ReadSearchHit>>> {
    let permit = state
        .trace
        .try_search_permit()
        .map_err(|error| TraceError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "search_work_budget",
            message: error.to_string(),
            response: None,
        })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(internal)?;
        runtime.block_on(search_inner(state, subject, query))
    })
    .await
    .map_err(internal)?
}
async fn search_inner(
    state: AppState,
    subject: SubjectId,
    query: SearchQuery,
) -> Result<Json<ReadPage<ReadSearchHit>>> {
    use crate::trace::search::{SearchError, SearchHistoryMode, SearchQueryContext, SearchScope};
    let error = |error: SearchError| match error {
        SearchError::Indexing => TraceError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "search_indexing",
            message: error.to_string(),
            response: None,
        },
        SearchError::Expired => TraceError {
            status: StatusCode::GONE,
            code: "snapshot_expired",
            message: error.to_string(),
            response: None,
        },
        SearchError::InvalidCursor => invalid(error.to_string()),
        SearchError::InvalidQuery(_) => TraceError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_search_query",
            message: error.to_string(),
            response: None,
        },
        SearchError::WorkBudget => TraceError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "search_work_budget",
            message: error.to_string(),
            response: None,
        },
        _ => internal(error),
    };
    let agent = query.agent.as_deref().map(self::agent).transpose()?;
    if let Some(agent) = &agent {
        observe(&state, &subject.0, agent).await?;
    }
    let active = state.trace.active().await.map_err(internal)?;
    let index = state
        .trace
        .search_store(&active.manifest().generation)
        .await
        .map_err(internal)?;
    let now = crate::models::now_ms();
    let max = query
        .limit
        .unwrap_or(32)
        .min(state.trace.limits().max_items);
    let mut environments = std::collections::BTreeSet::new();
    for environment in state
        .store
        .list_environment_rows()
        .await
        .map_err(internal)?
    {
        if state
            .store
            .has_capability(&subject.0, environment.id.as_str(), "observe")
            .await
            .map_err(internal)?
        {
            environments.insert(environment.id.to_string());
        }
    }
    let selection = query
        .selection
        .as_deref()
        .map(SelectionRef::decode)
        .transpose()
        .map_err(|error| invalid(error.to_string()))?;
    let scope = SearchScope {
        agent,
        mode: if selection.is_some() {
            SearchHistoryMode::SelectedContext
        } else {
            SearchHistoryMode::Authored
        },
        selection,
        representation: None,
    };
    let mut acquired_lease = None;
    let result = if let Some(cursor) = &query.cursor {
        index
            .resume_for(
                SearchQueryContext {
                    subject: &subject.0,
                    query: &query.q,
                    scope: &scope,
                    allowed_environments: &environments,
                },
                cursor,
                now,
                max,
            )
            .await
            .map_err(error)?
    } else {
        let snapshot = index
            .checkpoint()
            .await
            .map_err(error)?
            .ok_or_else(|| error(SearchError::Indexing))?;
        let lease = if let Some(token) = query.at_snapshot.as_deref() {
            let lease = lease(&state, &subject.0, Some(token)).await?;
            if lease.snapshot != snapshot {
                return Err(error(SearchError::Indexing));
            }
            lease
        } else {
            state
                .trace
                .lease_at_snapshot(&subject.0, &snapshot)
                .await
                .map_err(lease_error)?
        };
        let result = index
            .start_query(
                SearchQueryContext {
                    subject: &subject.0,
                    query: &query.q,
                    scope: &scope,
                    allowed_environments: &environments,
                },
                &snapshot,
                &lease.token,
                now,
                max,
            )
            .await
            .map_err(error)?;
        acquired_lease = Some(lease);
        result
    };
    let lease = lease(&state, &subject.0, Some(&result.trace_lease)).await?;
    let generation = state
        .trace
        .generation(&result.view.generation)
        .await
        .map_err(internal)?;
    let mut hits = Vec::new();
    for hit in result.hits {
        observe(&state, &subject.0, &hit.agent).await?;
        // Search metadata can contain a snippet before the containing header is
        // hydrated. Check every retained provenance dependency at this boundary;
        // a primary epoch alone cannot authorize a merged declaration or payload.
        for epoch in std::iter::once(&hit.ingestion_epoch)
            .chain(&hit.dependency_epochs)
            .collect::<std::collections::BTreeSet<_>>()
        {
            let proof = state
                .trace
                .catalogue()
                .scope_proof(epoch)
                .await
                .map_err(internal)?;
            if !state
                .store
                .has_capability(&subject.0, proof.scope.environment().as_str(), "observe")
                .await
                .map_err(internal)?
                || !state
                    .trace
                    .catalogue()
                    .scope_is_readable(&proof)
                    .await
                    .map_err(internal)?
            {
                return Err(storage(GenerationError::NotFound));
            }
            state
                .trace
                .catalogue()
                .bind_lease_scope(&subject.0, &lease.token, &proof, now)
                .await
                .map_err(internal)?;
        }
        let row = generation
            .read_row(
                &lease.snapshot,
                &read_scope(&hit.agent, None, "header"),
                &hit.agent.encode(),
            )
            .await
            .map_err(storage)?;
        check_row(&state, &subject.0, &lease, &row).await?;
        let header: AgentHeader = serde_json::from_value(row.body).map_err(internal)?;
        hits.push(ReadSearchHit {
            anchor: TraceAddress::of(&hit.anchor),
            agent: header.agent,
            history: HistoryLocation {
                selection: hit.selection,
                label: "Search checkpoint".into(),
                is_current: false,
                selection_known: true,
                availability: ReadAvailability::Available,
            },
            excerpt: hit.snippet,
            payload: None,
        });
    }
    if let Some(guard) = acquired_lease {
        guard.retain();
    }
    Ok(Json(ReadPage {
        view: read_view(&lease),
        returned_items: hits.len(),
        returned_bytes: serde_json::to_vec(&hits).map_err(internal)?.len(),
        items: hits,
        before_cursor: None,
        after_cursor: result.next_cursor,
        has_before: false,
        has_after: result.has_more,
        total: ReadCount {
            value: None,
            scope: "matching retained records".into(),
            complete: !result.result_limit_reached,
        },
        state: if result.result_limit_reached {
            ReadPageState::WorkBudgetExhausted
        } else if result.has_more {
            ReadPageState::More
        } else {
            ReadPageState::Complete
        },
        limits: state.trace.limits().clone(),
    }))
}

#[derive(Deserialize)]
struct ChangesQuery {
    interests: String,
    cursor: String,
    views: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InterestView {
    agent: AgentAddress,
    at_snapshot: String,
}
async fn changes(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Query(query): Query<ChangesQuery>,
) -> Result<Response> {
    if query.interests.len() > 64 * 1024
        || query.views.len() > 64 * 1024
        || query.cursor.len() > 1024
    {
        return Err(invalid("Change subscription metadata exceeds its limit"));
    }
    let interests: Vec<AgentAddress> =
        serde_json::from_str(&query.interests).map_err(|error| invalid(error.to_string()))?;
    if interests.is_empty() || interests.len() > state.trace.limits().max_visible_interests {
        return Err(invalid("Too many visible trace interests"));
    }
    for interest in &interests {
        observe(&state, &subject.0, &interest.agent()).await?;
    }
    let mut updates = state.trace.subscribe();
    let views: Vec<InterestView> =
        serde_json::from_str(&query.views).map_err(|error| invalid(error.to_string()))?;
    if views.is_empty() || views.len() > state.trace.limits().max_visible_interests {
        return Err(invalid("Too many visible trace views"));
    }
    if interests
        .iter()
        .any(|interest| !views.iter().any(|view| &view.agent == interest))
        || views.iter().any(|view| !interests.contains(&view.agent))
    {
        return Err(invalid(
            "Each visible interest requires its own retained view",
        ));
    }
    let mut scope_proofs = Vec::new();
    for view in &views {
        let lease = lease(&state, &subject.0, Some(&view.at_snapshot)).await?;
        let source_generation = state
            .trace
            .generation(&lease.snapshot.generation)
            .await
            .map_err(internal)?;
        let row = source_generation
            .read_row(
                &lease.snapshot,
                &read_scope(&view.agent.agent(), None, "header"),
                &view.agent.agent().encode(),
            )
            .await
            .map_err(storage)?;
        check_row(&state, &subject.0, &lease, &row).await?;
        scope_proofs.push(
            state
                .trace
                .catalogue()
                .scope_proof(&row.epoch)
                .await
                .map_err(internal)?,
        );
    }
    let (initial_generation, initial_revision) = query
        .cursor
        .rsplit_once(':')
        .filter(|(generation, _)| !generation.is_empty())
        .ok_or_else(|| invalid("Malformed trace change cursor"))?;
    let initial_revision = initial_revision
        .parse::<u64>()
        .map_err(|_| invalid("Malformed trace change revision"))?;
    let initial = (initial_generation.to_owned(), initial_revision);
    let stream = async_stream::stream! {
        let (previous_generation,mut previous_revision)=initial;let mut first=true;
        loop {
            if !first {tokio::select!{result=updates.changed()=>if result.is_err(){break;},_=tokio::time::sleep(std::time::Duration::from_secs(10))=>{}}}first=false;
            let mut authorized=true;
            for interest in &interests {if !state.store.has_capability(&subject.0,interest.agent().environment().as_str(),"observe").await.unwrap_or(false){authorized=false;break;}}
            if authorized {for proof in &scope_proofs {if !state.trace.catalogue().scope_is_readable(proof).await.unwrap_or(false){authorized=false;break;}}}
            if !authorized {
                let change=TraceChange::AccessChanged {authorization_scope:format!("{}:revoked",subject.0)};
                yield Ok::<_,std::convert::Infallible>(axum::response::sse::Event::default().json_data(change).unwrap());break;
            }
            let generation=match state.trace.active().await {Ok(generation)=>generation,Err(_)=>{yield Ok(axum::response::sse::Event::default().json_data(TraceChange::ResetRequired {reason:"Trace generation unavailable".into()}).unwrap());break;}};
            let checkpoint=match generation.checkpoint().await {Ok(checkpoint)=>checkpoint,Err(_)=>break};
            // A cursor certifies a complete retained prefix, not just a number
            // to compare with the latest revision. Missing generations, floors
            // and oversized catch-up batches require an explicit fresh window.
            let reset_reason=if checkpoint.snapshot.generation!=previous_generation {
                Some("Trace generation changed")
            } else if previous_revision<checkpoint.revision_floor || previous_revision>checkpoint.snapshot.view_revision {
                Some("Trace change cursor is outside the retained revision range")
            } else {None};
            if let Some(reason)=reset_reason {
                yield Ok(axum::response::sse::Event::default().json_data(TraceChange::ResetRequired {reason:reason.into()}).unwrap());break;
            }
            if previous_revision==checkpoint.snapshot.view_revision {continue;}
            let change=match replay_change(&generation,&checkpoint,previous_revision,&interests,state.trace.limits()).await {
                Ok(change)=>change,
                Err(reason)=>{yield Ok(axum::response::sse::Event::default().json_data(TraceChange::ResetRequired {reason}).unwrap());break;}
            };
            previous_revision=checkpoint.snapshot.view_revision;
            let cursor=format!("{}:{}",checkpoint.snapshot.generation,previous_revision);
            yield Ok(axum::response::sse::Event::default().id(cursor).json_data(change).unwrap());
        }
    };
    let mut response = axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response();
    // Next's proxy compresses an otherwise valid SSE response and buffers small
    // revisions indefinitely. This directive also applies to intervening proxies.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache, no-transform"),
    );
    response.headers_mut().insert(
        "x-accel-buffering",
        axum::http::HeaderValue::from_static("no"),
    );
    Ok(response)
}

async fn replay_change(
    generation: &crate::trace::generation::GenerationStore,
    checkpoint: &crate::trace::generation::Checkpoint,
    previous: u64,
    interests: &[AgentAddress],
    limits: &ReadLimits,
) -> std::result::Result<TraceChange, String> {
    let batches = generation
        .changes_after(previous, generation.limits().max_items)
        .await
        .map_err(|_| "Trace change backlog exceeds its retained bound".to_owned())?;
    let mut next = previous + 1;
    let mut changed = std::collections::BTreeSet::new();
    let mut removed = std::collections::BTreeSet::new();
    let mut moved = std::collections::BTreeSet::new();
    for batch in batches
        .into_iter()
        .take_while(|batch| batch.snapshot.view_revision <= checkpoint.snapshot.view_revision)
    {
        if batch.snapshot.view_revision != next {
            return Err("Trace change prefix is no longer retained".into());
        }
        next += 1;
        for (references, output) in [
            (&batch.changed, &mut changed),
            (&batch.removed, &mut removed),
            (&batch.moved, &mut moved),
        ] {
            for reference in references {
                if interests
                    .iter()
                    .any(|interest| &interest.agent() == reference.scope())
                {
                    output.insert(TraceAddress::of(reference));
                }
            }
        }
        if changed.len() + removed.len() + moved.len() > limits.max_change_items {
            return Err("Trace change item bound requires a fresh window".into());
        }
    }
    if next != checkpoint.snapshot.view_revision + 1 {
        return Err("Trace change backlog requires a fresh window".into());
    }
    let change = TraceChange::Revision {
        cursor: format!(
            "{}:{}",
            checkpoint.snapshot.generation, checkpoint.snapshot.view_revision
        ),
        generation: checkpoint.snapshot.generation.clone(),
        evidence_cut: checkpoint.snapshot.evidence_cut,
        view_revision: checkpoint.snapshot.view_revision,
        agents: interests.to_vec(),
        changed: changed.into_iter().collect(),
        removed: removed.into_iter().collect(),
        moved: moved.into_iter().collect(),
    };
    if serde_json::to_vec(&change)
        .map_err(|error| error.to_string())?
        .len()
        > limits.max_summary_bytes
    {
        return Err("Trace change byte bound requires a fresh window".into());
    }
    Ok(change)
}

#[derive(Default, Deserialize)]
pub struct ReadQuery {
    at_snapshot: Option<String>,
    selection: Option<String>,
    before: Option<String>,
    after: Option<String>,
    around: Option<String>,
    limit: Option<usize>,
    kind: Option<String>,
    q: Option<String>,
    filter: Option<String>,
    relation: Option<String>,
    direction: Option<String>,
}

fn agent(value: &str) -> Result<AgentRef> {
    AgentAddress::parse(value)
        .map(|address| address.agent())
        .map_err(|error| invalid(error.to_string()))
}
fn reference(value: &str) -> Result<AnyObjectRef> {
    TraceAddress::parse(value)
        .map(|address| address.object())
        .map_err(|error| invalid(error.to_string()))
}
fn read_view(lease: &ViewLease) -> ReadView {
    ReadView {
        token: lease.token.clone(),
        generation: lease.snapshot.generation.clone(),
        evidence_cut: lease.snapshot.evidence_cut,
        view_revision: lease.snapshot.view_revision,
        expires_at_ms: lease.expires_at_ms,
    }
}

async fn observe(state: &AppState, subject: &str, agent: &AgentRef) -> Result<()> {
    if !state
        .store
        .has_capability(subject, agent.environment().as_str(), "observe")
        .await
        .map_err(internal)?
    {
        return Err(TraceError {
            status: StatusCode::NOT_FOUND,
            code: "trace_object_not_found",
            message: "Trace object not found".into(),
            response: None,
        });
    }
    Ok(())
}

async fn lease(
    state: &AppState,
    subject: &str,
    token: Option<&str>,
) -> Result<crate::trace::service::LeaseGuard> {
    state
        .trace
        .lease_guard(subject, token)
        .await
        .map_err(lease_error)
}
fn lease_error(error: anyhow::Error) -> TraceError {
    if let Some(error) = error.downcast_ref::<crate::trace::service::TraceServiceError>() {
        if matches!(
            error,
            crate::trace::service::TraceServiceError::Recovering(_)
        ) {
            return TraceError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "projection_recovering",
                message: error.to_string(),
                response: None,
            };
        }
    }
    if let Some(error) = error.downcast_ref::<crate::trace::evidence::catalogue::CatalogueError>() {
        use crate::trace::evidence::catalogue::CatalogueError::*;
        if matches!(error, Limit(_)) {
            return TraceError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "read_capacity",
                message: "The active trace view limit has been reached".into(),
                response: None,
            };
        }

        if matches!(error, SnapshotExpired | DeletedScope) {
            return TraceError {
                status: StatusCode::GONE,
                code: "snapshot_expired",
                message: error.to_string(),
                response: None,
            };
        }
        if matches!(error, SubjectMismatch) {
            return TraceError {
                status: StatusCode::FORBIDDEN,
                code: "forbidden",
                message: error.to_string(),
                response: None,
            };
        }
    }
    internal(error)
}

async fn release_view(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(token): Path<String>,
) -> Result<StatusCode> {
    state
        .trace
        .release_lease(&subject.0, &token)
        .await
        .map_err(lease_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn check_row(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    row: &ReadRow,
) -> Result<()> {
    for environment in &row.required_environments {
        if !state
            .store
            .has_capability(subject, environment, "observe")
            .await
            .map_err(internal)?
        {
            return Err(storage(GenerationError::NotFound));
        }
    }
    for epoch in std::iter::once(&row.epoch)
        .chain(&row.dependency_epochs)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let proof = state
            .trace
            .catalogue()
            .scope_proof(epoch)
            .await
            .map_err(internal)?;
        if !state
            .store
            .has_capability(subject, proof.scope.environment().as_str(), "observe")
            .await
            .map_err(internal)?
        {
            return Err(storage(GenerationError::NotFound));
        }
        state
            .trace
            .catalogue()
            .bind_lease_scope(subject, &lease.token, &proof, crate::models::now_ms())
            .await
            .map_err(|error| lease_error(error.into()))?;
    }
    Ok(())
}

async fn authorized_environments(
    state: &AppState,
    subject: &str,
) -> Result<Option<std::collections::BTreeSet<String>>> {
    Ok(
        match state
            .store
            .capable_environments(subject, "observe")
            .await
            .map_err(internal)?
        {
            crate::db::metadata::CapabilityPartition::Unrestricted => None,
            crate::db::metadata::CapabilityPartition::Environments(environments) => {
                Some(environments)
            }
        },
    )
}

async fn candidate_is_readable(
    state: &AppState,
    subject: &str,
    candidate: ReadCandidate,
) -> std::result::Result<bool, GenerationError> {
    for epoch in std::iter::once(&candidate.epoch)
        .chain(&candidate.dependency_epochs)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let catalogue = state.trace.catalogue();
        let proof = catalogue
            .scope_proof(epoch)
            .await
            .map_err(|error| GenerationError::Invalid(error.to_string()))?;
        if !state
            .store
            .has_capability(subject, proof.scope.environment().as_str(), "observe")
            .await
            .map_err(|error| GenerationError::Invalid(error.to_string()))?
            || !catalogue
                .scope_is_readable(&proof)
                .await
                .map_err(|error| GenerationError::Invalid(error.to_string()))?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn cursor_error(error: crate::trace::cursor::CursorError) -> TraceError {
    match error {
        crate::trace::cursor::CursorError::InvalidCursor
        | crate::trace::cursor::CursorError::Limit => invalid("Invalid or mismatched read cursor"),
        crate::trace::cursor::CursorError::Crypto => internal(error),
    }
}

fn cursor_digest(scope: &str, filter: &ReadFilter, backward: bool) -> Result<String> {
    Ok(
        blake3::hash(&serde_json::to_vec(&(scope, filter, backward)).map_err(internal)?)
            .to_hex()
            .to_string(),
    )
}

fn seal_cursor(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    scope: &str,
    filter: &ReadFilter,
    backward: bool,
    position: Option<String>,
) -> Result<Option<String>> {
    position
        .map(|position| {
            let digest = cursor_digest(scope, filter, backward)?;
            state
                .trace
                .cursors()
                .seal(
                    &position,
                    crate::trace::cursor::CursorBinding {
                        domain: "sylph.reader.position",
                        subject,
                        lease: &lease.token,
                        query_digest: &digest,
                    },
                )
                .map_err(cursor_error)
        })
        .transpose()
}

fn open_cursor(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    scope: &str,
    filter: &ReadFilter,
    backward: bool,
    cursor: Option<&str>,
) -> Result<Option<String>> {
    cursor
        .map(|cursor| {
            let digest = cursor_digest(scope, filter, backward)?;
            state
                .trace
                .cursors()
                .open(
                    cursor,
                    crate::trace::cursor::CursorBinding {
                        domain: "sylph.reader.position",
                        subject,
                        lease: &lease.token,
                        query_digest: &digest,
                    },
                )
                .map_err(cursor_error)
        })
        .transpose()
}

async fn authorized_value(
    state: &AppState,
    subject: &str,
    generation: &crate::trace::generation::GenerationStore,
    snapshot: &crate::trace::generation::Snapshot,
    mut value: serde_json::Value,
) -> Result<serde_json::Value> {
    fn collect(value: &serde_json::Value, agents: &mut std::collections::BTreeSet<AgentRef>) {
        match value {
            serde_json::Value::Object(object) => {
                if object.contains_key("identitySuffix") && object.contains_key("vendor") {
                    if let Some(address) = object
                        .get("address")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|address| AgentAddress::parse(address).ok())
                    {
                        agents.insert(address.agent());
                    }
                }
                for value in object.values() {
                    collect(value, agents);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    collect(value, agents);
                }
            }
            _ => {}
        }
    }
    fn redact(value: &mut serde_json::Value, denied: &std::collections::BTreeSet<AgentRef>) {
        match value {
            serde_json::Value::Object(object) => {
                let forbidden = object.contains_key("identitySuffix")
                    && object.contains_key("vendor")
                    && object
                        .get("address")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|address| AgentAddress::parse(address).ok())
                        .is_some_and(|address| denied.contains(&address.agent()));
                if forbidden {
                    *value = serde_json::Value::Null;
                    return;
                }
                for value in object.values_mut() {
                    redact(value, denied);
                }
                if object.get("kind").and_then(serde_json::Value::as_str) == Some("agent")
                    && object.get("agent") == Some(&serde_json::Value::Null)
                {
                    object.insert(
                        "label".into(),
                        serde_json::Value::String("Agent · identity unavailable".into()),
                    );
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    redact(value, denied);
                }
            }
            _ => {}
        }
    }
    let mut agents = std::collections::BTreeSet::new();
    collect(&value, &mut agents);
    if agents.len() > generation.limits().max_candidates {
        return Err(storage(GenerationError::WorkBudget));
    }
    let mut denied = std::collections::BTreeSet::new();
    for agent in agents {
        if !state
            .store
            .has_capability(subject, agent.environment().as_str(), "observe")
            .await
            .map_err(internal)?
        {
            denied.insert(agent);
            continue;
        }
        let candidate = match generation
            .read_candidate(
                snapshot,
                &read_scope(&agent, None, "header"),
                &TraceAddress::of(&AnyObjectRef::Agent(agent.clone())),
            )
            .await
        {
            Ok(candidate) => candidate,
            Err(GenerationError::NotFound) => {
                denied.insert(agent);
                continue;
            }
            Err(error) => return Err(storage(error)),
        };
        // Check the epoch retained in this view, never the agent's current epoch:
        // re-adoption cannot authorize metadata from a deleted generation.
        if !candidate_is_readable(state, subject, candidate)
            .await
            .map_err(storage)?
        {
            denied.insert(agent);
        }
    }
    redact(&mut value, &denied);
    Ok(value)
}

fn selection(query: &ReadQuery) -> Result<Option<SelectionRef>> {
    query
        .selection
        .as_deref()
        .map(SelectionRef::decode)
        .transpose()
        .map_err(|error| invalid(error.to_string()))
}
async fn check_selected_object(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    selection: Option<&SelectionRef>,
    reference: &AnyObjectRef,
) -> Result<()> {
    let Some(selection) = selection else {
        return Ok(());
    };
    observe(state, subject, selection.scope()).await?;
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let row = generation
        .read_row(
            &lease.snapshot,
            &read_scope(selection.scope(), Some(selection), "selected-objects"),
            &reference.encode(),
        )
        .await
        .map_err(storage)?;
    check_row(state, subject, lease, &row).await
}
fn select_payloads(value: &mut serde_json::Value, selection: Option<&SelectionRef>) {
    let Some(selection) = selection else {
        return;
    };
    match value {
        serde_json::Value::Object(object) => {
            if object.contains_key("payloadRef")
                && object.contains_key("viaRef")
                && object.contains_key("bodyRevision")
            {
                if object
                    .get("containingAgent")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| AgentAddress::parse(value).ok())
                    .is_some_and(|agent| &agent.agent() == selection.scope())
                {
                    object.insert(
                        "selection".into(),
                        serde_json::Value::String(selection.encode()),
                    );
                }
            }
            for value in object.values_mut() {
                select_payloads(value, Some(selection));
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                select_payloads(value, Some(selection));
            }
        }
        _ => {}
    }
}

pub async fn bootstrap(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<ReaderBootstrap>> {
    let agent = agent(&encoded)?;
    observe(&state, &subject.0, &agent).await?;
    let lease = lease(&state, &subject.0, query.at_snapshot.as_deref()).await?;
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let row = generation
        .read_row(
            &lease.snapshot,
            &read_scope(&agent, None, "header"),
            &agent.encode(),
        )
        .await;
    let row = match row {
        Ok(row) => row,
        Err(GenerationError::NotFound)
            if state
                .trace
                .evidence()
                .evidence_cut()
                .await
                .map_err(internal)?
                > lease.snapshot.evidence_cut =>
        {
            return Err(TraceError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "projection_pending",
                message: "Updating retained history".into(),
                response: None,
            });
        }
        Err(error) => return Err(storage(error)),
    };
    check_row(&state, &subject.0, &lease, &row).await?;
    let mut header: AgentHeader = serde_json::from_value(
        authorized_value(&state, &subject.0, &generation, &lease.snapshot, row.body).await?,
    )
    .map_err(internal)?;
    let count_filter = ReadFilter {
        allowed_environments: authorized_environments(&state, &subject.0).await?,
        ..ReadFilter::default()
    };
    let count = generation
        .read_authorized_count(
            &lease.snapshot,
            &read_scope(&agent, None, "relations"),
            &count_filter,
            |candidate| candidate_is_readable(&state, &subject.0, candidate),
        )
        .await
        .map_err(storage)?;
    header.descendant_count = ReadCount {
        value: count,
        scope: "authorized direct spawned agents".into(),
        complete: count.is_some(),
    };
    if let AgentRef::Session(session_id) = &agent {
        if let Some(session) = state
            .store
            .get_session(session_id)
            .await
            .map_err(internal)?
        {
            header.file_context = session.workdir.map(|workdir| ReadFileContext {
                session_id: session_id.encode(),
                environment: session_id.environment.as_str().into(),
                workdir,
            });
        }
    }
    let selection = query
        .selection
        .as_deref()
        .map(SelectionRef::decode)
        .transpose()
        .map_err(|error| invalid(error.to_string()))?
        .or_else(|| header.history.selection.clone());
    if let Some(selected) = selection.as_ref() {
        if header.history.selection.as_ref() != Some(selected) {
            let row = generation
                .read_row(
                    &lease.snapshot,
                    &read_scope(&agent, None, "histories"),
                    &selected.encode(),
                )
                .await
                .map_err(storage)?;
            check_row(&state, &subject.0, &lease, &row).await?;
            let choice: HistoryChoice = serde_json::from_value(row.body).map_err(internal)?;
            header.history = HistoryLocation {
                selection: Some(choice.selection),
                label: choice.label,
                is_current: choice.is_current,
                selection_known: true,
                availability: choice.availability,
            };
        }
    }
    let mut window = page::<ReadingUnit>(
        &state,
        &subject.0,
        &lease,
        &agent,
        selection.as_ref(),
        "history",
        &query,
        true,
    )
    .await?;
    hydrate_parts(&state, &subject.0, &mut window, &lease).await?;
    let progress = state.trace.progress().await;
    let outbox = state
        .store
        .progress(crate::models::now_ms())
        .await
        .map_err(internal)?;
    let control = crate::api::controls::control_availability(&state, &subject.0, &agent)
        .await
        .map_err(operational)?;
    #[cfg(test)]
    if let Some((entered, resume)) = state.trace.bootstrap_response_pause.lock().await.take() {
        entered.notify_one();
        resume.notified().await;
    }
    let response = ReaderBootstrap {
        view: read_view(&lease),
        header,
        window,
        change_cursor: format!(
            "{}:{}",
            lease.snapshot.generation, lease.snapshot.view_revision
        ),
        progress: TraceProgress {
            evidence_cut: state
                .trace
                .evidence()
                .evidence_cut()
                .await
                .map_err(internal)?,
            projection_cut: lease.snapshot.evidence_cut,
            search_cut: None,
            pending_outbox: outbox.pending_count,
            oldest_outbox_age_ms: outbox.oldest_pending_age_ms,
            source_connected: matches!(control, ReadControlAvailability::Available { .. }),
            fault: progress.fault,
        },
        control,
        authorization_scope: subject.0.clone(),
    };
    // Ownership moves only after every fallible read completes. Cancellation
    // during response assembly still leaves the guard responsible for cleanup.
    lease.retain();
    Ok(Json(response))
}

async fn page<T: serde::de::DeserializeOwned + Serialize>(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    agent: &AgentRef,
    selection: Option<&SelectionRef>,
    collection: &str,
    query: &ReadQuery,
    latest: bool,
) -> Result<ReadPage<T>> {
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let around = query
        .around
        .as_deref()
        .map(TraceAddress::parse)
        .transpose()
        .map_err(|error| invalid(error.to_string()))?;
    let mut filter = if collection == "relations" {
        if query.q.as_ref().is_some_and(|text| text.len() > 512) {
            return Err(invalid("Directory search exceeds its limit"));
        }
        if query
            .relation
            .as_deref()
            .is_some_and(|relation| relation != "spawn")
            || query
                .direction
                .as_deref()
                .is_some_and(|direction| direction != "outgoing")
        {
            return Err(invalid("Unknown directory relation scope"));
        }
        let state = query.filter.as_deref().unwrap_or("all");
        if !matches!(
            state,
            "all" | "active" | "needs_input" | "completed" | "unknown" | "failed" | "cancelled"
        ) {
            return Err(invalid("Unknown directory filter"));
        }
        ReadFilter {
            text: query.q.as_deref().unwrap_or("").trim().to_lowercase(),
            state: if state == "all" {
                String::new()
            } else {
                state.into()
            },
            ..ReadFilter::default()
        }
    } else {
        ReadFilter::default()
    };
    filter.allowed_environments = authorized_environments(state, subject).await?;
    let scope = read_scope(agent, selection, collection);
    let before = open_cursor(
        state,
        subject,
        lease,
        &scope,
        &filter,
        true,
        query.before.as_deref(),
    )?;
    let after = open_cursor(
        state,
        subject,
        lease,
        &scope,
        &filter,
        false,
        query.after.as_deref(),
    )?;
    let rows = generation
        .read_authorized_page(
            &lease.snapshot,
            &scope,
            ReadWindow {
                before: before.as_deref(),
                after: after.as_deref(),
                around: around.as_ref(),
                limit: query.limit.unwrap_or(state.trace.limits().max_items),
                latest,
            },
            &filter,
            |candidate| candidate_is_readable(state, subject, candidate),
        )
        .await
        .map_err(storage)?;
    let mut items = Vec::new();
    for row in rows.rows {
        check_row(state, subject, lease, &row).await?;
        let mut body =
            authorized_value(state, subject, &generation, &lease.snapshot, row.body).await?;
        select_payloads(&mut body, selection);
        items.push(serde_json::from_value(body).map_err(internal)?);
    }
    let bytes = serde_json::to_vec(&items).map_err(internal)?.len();
    Ok(ReadPage {
        view: read_view(lease),
        returned_items: items.len(),
        returned_bytes: bytes,
        items,
        before_cursor: seal_cursor(state, subject, lease, &scope, &filter, true, rows.before)?,
        after_cursor: seal_cursor(state, subject, lease, &scope, &filter, false, rows.after)?,
        has_before: rows.has_before,
        has_after: rows.has_after,
        total: ReadCount {
            value: rows.total,
            scope: if collection == "relations" {
                "authorized direct spawned agents"
            } else {
                collection
            }
            .into(),
            complete: rows.total.is_some(),
        },
        state: if rows.work_exhausted {
            ReadPageState::WorkBudgetExhausted
        } else if rows.has_after || rows.has_before {
            ReadPageState::More
        } else {
            ReadPageState::Complete
        },
        limits: state.trace.limits().clone(),
    })
}

async fn history(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<ReadPage<ReadingUnit>>> {
    let agent = agent(&encoded)?;
    observe(&state, &subject.0, &agent).await?;
    let lease = lease(&state, &subject.0, query.at_snapshot.as_deref()).await?;
    let selection = query
        .selection
        .as_deref()
        .map(SelectionRef::decode)
        .transpose()
        .map_err(|error| invalid(error.to_string()))?;
    let mut page = page(
        &state,
        &subject.0,
        &lease,
        &agent,
        selection.as_ref(),
        "history",
        &query,
        true,
    )
    .await?;
    hydrate_parts(&state, &subject.0, &mut page, &lease).await?;
    lease.retain();
    Ok(Json(page))
}

async fn hydrate_parts(
    state: &AppState,
    subject: &str,
    page: &mut ReadPage<ReadingUnit>,
    lease: &ViewLease,
) -> Result<()> {
    let filter = ReadFilter {
        allowed_environments: authorized_environments(state, subject).await?,
        ..ReadFilter::default()
    };
    for unit in &mut page.items {
        if let ReadingContent::Message {
            revision_ref,
            parts,
            parts_complete,
            parts_cursor,
            ..
        } = &mut unit.content
        {
            if !*parts_complete {
                if let Some(last) = parts.last() {
                    let scope = sylph_trace::reading::parts_key(
                        &last.payload.containing_agent.agent(),
                        last.payload.selection.as_ref(),
                        &unit.anchor.object(),
                        revision_ref,
                    );
                    *parts_cursor = seal_cursor(
                        state,
                        subject,
                        lease,
                        &scope,
                        &filter,
                        false,
                        Some(
                            crate::trace::reading_store::cursor_after(
                                &lease.snapshot,
                                &scope,
                                &last.part_ref.encode(),
                                parts.len() - 1,
                                &filter,
                            )
                            .map_err(storage)?,
                        ),
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct PartsQuery {
    at_snapshot: String,
    via_ref: String,
    selection: Option<SelectionRef>,
    after: Option<String>,
    limit: Option<usize>,
}
async fn parts(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<PartsQuery>,
) -> Result<Json<ReadPage<PartSummary>>> {
    let AnyObjectRef::MessageRevision(revision) = reference(&encoded)? else {
        return Err(invalid("Parts belong to a message revision"));
    };
    let anchor = reference(&query.via_ref)?;
    observe(&state, &subject.0, anchor.scope()).await?;
    let lease = lease(&state, &subject.0, Some(&query.at_snapshot)).await?;
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let scope = sylph_trace::reading::parts_key(
        anchor.scope(),
        query.selection.as_ref(),
        &anchor,
        &revision,
    );
    let filter = ReadFilter {
        allowed_environments: authorized_environments(&state, &subject.0).await?,
        ..ReadFilter::default()
    };
    let after = open_cursor(
        &state,
        &subject.0,
        &lease,
        &scope,
        &filter,
        false,
        query.after.as_deref(),
    )?;
    let rows = generation
        .read_authorized_page(
            &lease.snapshot,
            &scope,
            ReadWindow {
                before: None,
                after: after.as_deref(),
                around: None,
                limit: query
                    .limit
                    .unwrap_or(32)
                    .min(state.trace.limits().max_items),
                latest: false,
            },
            &filter,
            |candidate| candidate_is_readable(&state, &subject.0, candidate),
        )
        .await
        .map_err(storage)?;
    let mut items = Vec::new();
    for row in rows.rows {
        check_row(&state, &subject.0, &lease, &row).await?;
        items.push(serde_json::from_value::<PartSummary>(row.body).map_err(internal)?);
    }
    Ok(Json(ReadPage {
        view: read_view(&lease),
        returned_items: items.len(),
        returned_bytes: serde_json::to_vec(&items).map_err(internal)?.len(),
        items,
        before_cursor: seal_cursor(
            &state,
            &subject.0,
            &lease,
            &scope,
            &filter,
            true,
            rows.before,
        )?,
        after_cursor: seal_cursor(
            &state, &subject.0, &lease, &scope, &filter, false, rows.after,
        )?,
        has_before: rows.has_before,
        has_after: rows.has_after,
        total: ReadCount {
            value: rows.total,
            scope: "ordered message parts".into(),
            complete: rows.total.is_some(),
        },
        state: if rows.work_exhausted {
            ReadPageState::WorkBudgetExhausted
        } else if rows.has_after {
            ReadPageState::More
        } else {
            ReadPageState::Complete
        },
        limits: state.trace.limits().clone(),
    }))
}

async fn relations(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<ReadPage<AgentDirectoryRow>>> {
    let agent = agent(&encoded)?;
    observe(&state, &subject.0, &agent).await?;
    let lease = lease(&state, &subject.0, query.at_snapshot.as_deref()).await?;
    let result = page(
        &state,
        &subject.0,
        &lease,
        &agent,
        None,
        "relations",
        &query,
        false,
    )
    .await?;
    lease.retain();
    Ok(Json(result))
}
async fn activations(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<ReadPage<ExecutionSummary>>> {
    let agent = agent(&encoded)?;
    observe(&state, &subject.0, &agent).await?;
    if query.selection.is_some() {
        return Err(invalid(
            "The activation catalogue covers all recorded own activations",
        ));
    }
    let lease = lease(&state, &subject.0, query.at_snapshot.as_deref()).await?;
    let mut result = page(
        &state,
        &subject.0,
        &lease,
        &agent,
        None,
        "activations",
        &query,
        false,
    )
    .await?;
    result.total.scope = "own activations".into();
    lease.retain();
    Ok(Json(result))
}

async fn histories(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<ReadPage<HistoryChoice>>> {
    let agent = agent(&encoded)?;
    observe(&state, &subject.0, &agent).await?;
    let lease = lease(&state, &subject.0, query.at_snapshot.as_deref()).await?;
    let result = page(
        &state,
        &subject.0,
        &lease,
        &agent,
        None,
        "histories",
        &query,
        false,
    )
    .await?;
    lease.retain();
    Ok(Json(result))
}
async fn control(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
) -> Result<Json<ReadControlAvailability>> {
    let agent = agent(&encoded)?;
    observe(&state, &subject.0, &agent).await?;
    Ok(Json(
        crate::api::controls::control_availability(&state, &subject.0, &agent)
            .await
            .map_err(operational)?,
    ))
}
async fn command(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Json(request): Json<ReadCommandRequest>,
) -> Result<(StatusCode, Json<ReadCommandReceipt>)> {
    Ok((
        StatusCode::ACCEPTED,
        Json(
            crate::api::controls::submit_trace(&state, &subject.0, request)
                .await
                .map_err(operational)?,
        ),
    ))
}
async fn command_list(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let agent = agent(&encoded)?;
    observe(&state, &subject.0, &agent).await?;
    Ok(Json(
        serde_json::json!({"commands":crate::api::controls::trace_command_list(&state,&subject.0,&agent).await.map_err(operational)?}),
    ))
}

async fn object(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<ReadObjectDetail>> {
    let reference = reference(&encoded)?;
    observe(&state, &subject.0, reference.scope()).await?;
    let lease = lease(&state, &subject.0, query.at_snapshot.as_deref()).await?;
    let selected = selection(&query)?;
    check_selected_object(&state, &subject.0, &lease, selected.as_ref(), &reference).await?;
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let row = generation
        .read_row(
            &lease.snapshot,
            &read_scope(reference.scope(), None, "objects"),
            &reference.encode(),
        )
        .await
        .map_err(storage)?;
    check_row(&state, &subject.0, &lease, &row).await?;
    let mut body =
        authorized_value(&state, &subject.0, &generation, &lease.snapshot, row.body).await?;
    select_payloads(&mut body, selected.as_ref());
    let mut object: ReadObjectDetail = serde_json::from_value(body).map_err(internal)?;
    object.view = read_view(&lease);
    let window = ReadQuery {
        limit: Some(8),
        ..ReadQuery::default()
    };
    let relations: ReadPage<ReadRelation> = page(
        &state,
        &subject.0,
        &lease,
        reference.scope(),
        selected.as_ref(),
        &format!("object-relations:{}", reference.encode()),
        &window,
        false,
    )
    .await?;
    object.relations = relations.items;
    object.relations_cursor = relations.after_cursor;
    match &mut object.content {
        ReadObjectContent::Operation {
            results,
            results_cursor,
            resources,
            resources_cursor,
            ..
        } => {
            let page: ReadPage<ReadResultOccurrence> = page(
                &state,
                &subject.0,
                &lease,
                reference.scope(),
                selected.as_ref(),
                &format!("object-results:{}", reference.encode()),
                &window,
                false,
            )
            .await?;
            *results = page.items;
            *results_cursor = page.after_cursor;
            let page: ReadPage<ReadRelation> = self::page(
                &state,
                &subject.0,
                &lease,
                reference.scope(),
                selected.as_ref(),
                &format!("object-resources:{}", reference.encode()),
                &window,
                false,
            )
            .await?;
            *resources = page.items;
            *resources_cursor = page.after_cursor;
        }
        ReadObjectContent::Context {
            memberships,
            memberships_cursor,
            ..
        } => {
            let page: ReadPage<ReadRelation> = page(
                &state,
                &subject.0,
                &lease,
                reference.scope(),
                selected.as_ref(),
                &format!("object-memberships:{}", reference.encode()),
                &window,
                false,
            )
            .await?;
            *memberships = page.items;
            *memberships_cursor = page.after_cursor;
        }
        _ => {}
    }
    lease.retain();
    Ok(Json(object))
}
async fn object_relations(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Response> {
    let reference = reference(&encoded)?;
    observe(&state, &subject.0, reference.scope()).await?;
    let lease = lease(&state, &subject.0, query.at_snapshot.as_deref()).await?;
    let selected = selection(&query)?;
    check_selected_object(&state, &subject.0, &lease, selected.as_ref(), &reference).await?;
    let kind = query.kind.as_deref().unwrap_or("relations");
    if !matches!(kind, "relations" | "results" | "resources" | "memberships") {
        return Err(invalid("Unknown relation collection"));
    }
    let scope = format!("object-{kind}:{}", reference.encode());
    let response: Result<Response> = if kind == "results" {
        Ok(Json(
            page::<ReadResultOccurrence>(
                &state,
                &subject.0,
                &lease,
                reference.scope(),
                selected.as_ref(),
                &scope,
                &query,
                false,
            )
            .await?,
        )
        .into_response())
    } else {
        Ok(Json(
            page::<ReadRelation>(
                &state,
                &subject.0,
                &lease,
                reference.scope(),
                selected.as_ref(),
                &scope,
                &query,
                false,
            )
            .await?,
        )
        .into_response())
    };
    let response = response?;
    lease.retain();
    Ok(response)
}

#[derive(Deserialize)]
struct PayloadQuery {
    via_ref: String,
    at_snapshot: String,
    revision: String,
    representation: Representation,
    selection: Option<SelectionRef>,
    start: Option<u64>,
    end: Option<u64>,
}
async fn payload(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    Query(query): Query<PayloadQuery>,
) -> Result<Response> {
    let payload = PayloadRef::parse(encoded).map_err(|error| invalid(error.to_string()))?;
    let anchor = reference(&query.via_ref)?;
    observe(&state, &subject.0, anchor.scope()).await?;
    let lease = lease(&state, &subject.0, Some(&query.at_snapshot)).await?;
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let row = generation
        .read_row(
            &lease.snapshot,
            &read_scope(anchor.scope(), None, "objects"),
            &anchor.encode(),
        )
        .await
        .map_err(storage)?;
    check_row(&state, &subject.0, &lease, &row).await?;
    let detail = generation
        .object(&lease.snapshot, &anchor)
        .await
        .map_err(storage)?;
    let owner = match detail.object {
        TraceObject::HistoryEntry(entry) => entry.containing_agent,
        TraceObject::Delivery(delivery) => match delivery.recipient {
            Recipient::Agent {
                agent: OwnerRef::Agent { agent },
            } => agent,
            _ => return Err(invalid("Delivery has no agent recipient")),
        },
        TraceObject::Operation(operation) => match operation.owner {
            OwnerRef::Agent { agent } => agent,
            _ => return Err(invalid("Operation owner unresolved")),
        },
        _ => {
            return Err(invalid(
                "Payload authorization needs an entry, delivery or operation",
            ))
        }
    };
    observe(&state, &subject.0, &owner).await?;
    let requested = crate::trace::generation::PayloadAnchor {
        via_ref: anchor,
        containing_agent: owner,
        selection: query.selection,
        payload_ref: payload.clone(),
        body_revision: query.revision,
        representation: query.representation,
        ranges: vec![],
    };
    let authorization_start = query.start.unwrap_or(0);
    generation
        .authorize_payload(
            &lease.snapshot,
            &requested,
            authorization_start,
            query.end.unwrap_or(authorization_start),
        )
        .await
        .map_err(storage)?;
    let total = state
        .trace
        .evidence()
        .payload_length(&payload)
        .await
        .map_err(internal)?;
    let start = query.start.unwrap_or(0);
    let end = query.end.unwrap_or_else(|| {
        total
            .unwrap_or(start)
            .min(start.saturating_add(state.trace.limits().max_payload_bytes as u64))
    });
    if end < start || end - start > state.trace.limits().max_payload_bytes as u64 {
        return Err(invalid("Payload range exceeds the configured limit"));
    }
    generation
        .authorize_payload(&lease.snapshot, &requested, start, end)
        .await
        .map_err(storage)?;
    let bytes = if end == start {
        vec![]
    } else {
        state
            .trace
            .evidence()
            .payload_range(&payload, start, (end - start) as usize)
            .await
            .map_err(internal)?
            .bytes
    };
    check_row(&state, &subject.0, &lease, &row).await?;
    let derivations = generation
        .payload_derivations(&lease.snapshot, &payload, 1)
        .await
        .map_err(storage)?;
    let mime = derivations
        .first()
        .map_or("application/octet-stream", |value| {
            value.media_type.as_str()
        });
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_str(mime).map_err(internal)?,
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    if end > start {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            axum::http::HeaderValue::from_str(&format!(
                "bytes {}-{}/{}",
                start,
                end - 1,
                total.map_or_else(|| "*".into(), |total| total.to_string())
            ))
            .map_err(internal)?,
        );
    }
    Ok(response)
}
