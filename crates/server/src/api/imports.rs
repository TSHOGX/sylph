//! Durable import scope, discovery snapshot, and per-session outcomes.

use std::{collections::BTreeMap, convert::Infallible, time::Duration};

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sylph_protocol::{
    AgentName, Command, CommandOutcome, CommandRequestId, EnvironmentCommand, EnvironmentId,
    IdempotencyPolicy, NativeSummary, SessionId,
};

use crate::{
    error::{Error, Result},
    middleware::auth::SubjectId,
    models::{now_ms, AdoptSessionRequest},
    AppState,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    ImportOnce,
    ImportAndWatch,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateImportRequest {
    pub workdir: Option<String>,
    pub agents: Option<Vec<String>>,
    pub mode: ImportMode,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportJob {
    pub id: String,
    pub environment: String,
    pub origin_subject: String,
    pub workdir: Option<String>,
    pub mode: String,
    pub status: String,
    pub scan_complete: bool,
    pub cancel_requested: bool,
    pub total: i64,
    pub imported: i64,
    pub refreshed: i64,
    pub skipped: i64,
    pub failed: i64,
    pub pending: i64,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl ImportJob {
    fn active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running")
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ImportItem {
    pub ordinal: i64,
    pub agent: String,
    pub native_id: String,
    pub title: Option<String>,
    pub workdir: Option<String>,
    pub status: String,
    pub reason: Option<String>,
    pub error_code: Option<String>,
    #[serde(skip)]
    pub source_revision: Option<String>,
}

const HISTORY_CONCURRENCY: usize = 2;
const SYNC_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
pub struct ImportResponse {
    pub job: ImportJob,
}

const JOB_QUERY: &str = "SELECT j.id, j.environment_id AS environment, j.origin_subject,
    j.workdir, j.mode, j.status, j.scan_complete, j.cancel_requested, j.error,
    j.created_at_ms, j.updated_at_ms, COUNT(i.ordinal) AS total,
    COALESCE(SUM(i.status = 'imported'),0) AS imported,
    COALESCE(SUM(i.status = 'refreshed'),0) AS refreshed,
    COALESCE(SUM(i.status = 'skipped'),0) AS skipped,
    COALESCE(SUM(i.status = 'failed'),0) AS failed,
    COALESCE(SUM(i.status IN ('pending','running')),0) AS pending
    FROM import_jobs j LEFT JOIN import_items i ON i.job_id = j.id";

async fn require_capability(
    state: &AppState,
    subject: &SubjectId,
    environment: &str,
    capability: &str,
) -> Result<()> {
    if !state
        .store
        .has_capability(&subject.0, environment, capability)
        .await?
    {
        return Err(Error::Forbidden(format!(
            "{capability} capability required"
        )));
    }
    Ok(())
}

async fn job(state: &AppState, id: &str) -> Result<ImportJob> {
    sqlx::query_as::<_, ImportJob>(&format!("{JOB_QUERY} WHERE j.id = ? GROUP BY j.id"))
        .bind(id)
        .fetch_optional(state.store.raw_pool())
        .await
        .map_err(Error::Database)?
        .ok_or(Error::RequestNotFound)
}

async fn authorized_job(
    state: &AppState,
    subject: &SubjectId,
    id: &str,
    capability: &str,
) -> Result<ImportJob> {
    let job = job(state, id).await?;
    require_capability(state, subject, &job.environment, capability).await?;
    Ok(job)
}

pub async fn create(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(environment): Path<String>,
    Json(mut req): Json<CreateImportRequest>,
) -> Result<(StatusCode, Json<ImportResponse>)> {
    EnvironmentId::parse(environment.clone()).map_err(|e| Error::InvalidRequest(e.to_string()))?;
    require_capability(&state, &subject, &environment, "control").await?;
    req.idempotency_key = req.idempotency_key.trim().to_string();
    if req.idempotency_key.is_empty() || req.idempotency_key.len() > 128 {
        return Err(Error::InvalidRequest(
            "idempotencyKey must contain 1 to 128 bytes".into(),
        ));
    }
    req.workdir = req.workdir.map(|p| super::workspaces::normalize(p.trim()));
    if req.workdir.as_ref().is_some_and(|p| !p.starts_with('/')) {
        return Err(Error::InvalidRequest(
            "workdir must be an absolute path".into(),
        ));
    }
    if let Some(agents) = &mut req.agents {
        if agents.is_empty() || agents.len() > 16 {
            return Err(Error::InvalidRequest(
                "agents must contain 1 to 16 names".into(),
            ));
        }
        for agent in agents.iter() {
            AgentName::parse(agent.clone()).map_err(|e| Error::InvalidRequest(e.to_string()))?;
        }
        agents.sort();
        agents.dedup();
    }
    let request_json = serde_json::to_string(&req).map_err(|e| Error::Internal(e.to_string()))?;
    let id = format!("import-{}", uuid::Uuid::new_v4().simple());
    let mode = match req.mode {
        ImportMode::ImportOnce => "import_once",
        ImportMode::ImportAndWatch => "import_and_watch",
    };
    // The unique key arbitrates racing clients. Read the winning row, including its
    // payload, before dispatch: an idempotency key cannot silently change its scope.
    let created = sqlx::query("INSERT INTO import_jobs (id,environment_id,origin_subject,workdir,agents_json,mode,idempotency_key,request_json,status,created_at_ms,updated_at_ms)
        VALUES (?,?,?,?,?,?,?,?, 'queued', ?, ?) ON CONFLICT(environment_id,origin_subject,idempotency_key) DO NOTHING")
        .bind(&id).bind(&environment).bind(&subject.0).bind(&req.workdir)
        .bind(req.agents.as_ref().map(serde_json::to_string).transpose().map_err(|e| Error::Internal(e.to_string()))?)
        .bind(mode).bind(&req.idempotency_key).bind(&request_json).bind(now_ms()).bind(now_ms())
        .execute(state.store.raw_pool()).await.map_err(Error::Database)?.rows_affected() > 0;
    let (stored_id, stored_request): (String, String) = sqlx::query_as("SELECT id,request_json FROM import_jobs WHERE environment_id=? AND origin_subject=? AND idempotency_key=?")
        .bind(&environment).bind(&subject.0).bind(&req.idempotency_key).fetch_one(state.store.raw_pool()).await.map_err(Error::Database)?;
    if stored_request != request_json {
        return Err(Error::InvalidRequest(
            "idempotencyKey already names an import with different options".into(),
        ));
    }
    let response = job(&state, &stored_id).await?;
    if created {
        spawn(state, stored_id);
    }
    Ok((StatusCode::ACCEPTED, Json(ImportResponse { job: response })))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    workdir: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(environment): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<serde_json::Value>> {
    require_capability(&state, &subject, &environment, "observe").await?;
    let jobs = sqlx::query_as::<_, ImportJob>(&format!("{JOB_QUERY} WHERE j.environment_id=? AND (? IS NULL OR j.workdir=?) GROUP BY j.id ORDER BY j.rowid DESC LIMIT 20"))
        .bind(environment).bind(&query.workdir).bind(&query.workdir).fetch_all(state.store.raw_pool()).await.map_err(Error::Database)?;
    Ok(Json(serde_json::json!({"jobs": jobs})))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<ImportResponse>> {
    Ok(Json(ImportResponse {
        job: authorized_job(&state, &subject, &id, "observe").await?,
    }))
}

#[derive(Default, Deserialize)]
pub struct ItemsQuery {
    after: Option<i64>,
    limit: Option<i64>,
}

pub async fn items(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<serde_json::Value>> {
    authorized_job(&state, &subject, &id, "observe").await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let mut rows = sqlx::query_as::<_, ImportItem>("SELECT ordinal,agent,native_id,title,workdir,status,reason,error_code,source_revision FROM import_items WHERE job_id=? AND ordinal>? ORDER BY ordinal LIMIT ?")
        .bind(id).bind(query.after.unwrap_or(-1)).bind(limit + 1).fetch_all(state.store.raw_pool()).await.map_err(Error::Database)?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = if has_more {
        rows.last().map(|r| r.ordinal)
    } else {
        None
    };
    Ok(Json(
        serde_json::json!({"items": rows, "nextCursor": next_cursor}),
    ))
}

pub async fn cancel(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<ImportResponse>> {
    authorized_job(&state, &subject, &id, "control").await?;
    sqlx::query("UPDATE import_jobs SET cancel_requested=1, status=CASE WHEN status='queued' THEN 'cancelled' ELSE status END, updated_at_ms=? WHERE id=? AND status IN ('queued','running')")
        .bind(now_ms()).bind(&id).execute(state.store.raw_pool()).await.map_err(Error::Database)?;
    Ok(Json(ImportResponse {
        job: job(&state, &id).await?,
    }))
}

pub async fn retry(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<ImportResponse>> {
    authorized_job(&state, &subject, &id, "control").await?;
    let mut tx = state
        .store
        .raw_pool()
        .begin()
        .await
        .map_err(Error::Database)?;
    let changed = sqlx::query("UPDATE import_jobs SET status='queued', cancel_requested=0,error=NULL,updated_at_ms=? WHERE id=? AND status IN ('failed','cancelled','completed')")
        .bind(now_ms()).bind(&id).execute(&mut *tx).await.map_err(Error::Database)?.rows_affected() > 0;
    if changed {
        sqlx::query("UPDATE import_items SET status='pending',reason=NULL,error_code=NULL WHERE job_id=? AND status IN ('failed','running')")
            .bind(&id).execute(&mut *tx).await.map_err(Error::Database)?;
    }
    tx.commit().await.map_err(Error::Database)?;
    let response = job(&state, &id).await?;
    if changed {
        spawn(state, id);
    }
    Ok(Json(ImportResponse { job: response }))
}

pub async fn events(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Sse<impl futures::Stream<Item = std::result::Result<Event, Infallible>>>> {
    authorized_job(&state, &subject, &id, "observe").await?;
    let stream = async_stream::stream! {
        let mut previous = String::new();
        loop {
            let current = match authorized_job(&state, &subject, &id, "observe").await { Ok(job) => job, Err(_) => break };
            let done = !current.active();
            let data = match serde_json::to_string(&ImportResponse { job: current }) { Ok(data) => data, Err(_) => break };
            if data != previous { yield Ok(Event::default().event("import").data(data.clone())); previous = data; }
            if done { break; }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// A server restart cannot leave a job claiming that a vanished worker is running.
/// Completed item identities stay durable, and retry resumes the remaining snapshot.
pub async fn recover_state(store: &crate::db::JournalStore) -> Result<()> {
    sqlx::query("UPDATE import_jobs SET status='failed',error='Server restarted; retry to resume remaining history',updated_at_ms=? WHERE status IN ('queued','running')")
        .bind(now_ms()).execute(store.raw_pool()).await.map_err(Error::Database)?;
    Ok(())
}

pub fn spawn_sync(state: AppState) {
    tokio::spawn(async move {
        let mut failed = false;
        loop {
            tokio::time::sleep(SYNC_INTERVAL).await;
            match sync_once(&state).await {
                Ok(_) => failed = false,
                Err(error) => {
                    if failed {
                        tracing::debug!(%error, "native history synchronization is unavailable");
                    } else {
                        tracing::error!(%error, "native history synchronization is unavailable");
                        failed = true;
                    }
                }
            }
        }
    });
}

#[derive(FromRow)]
struct SyncItem {
    job_id: String,
    environment: String,
    origin_subject: String,
    #[sqlx(flatten)]
    item: ImportItem,
}

async fn sync_candidates(store: &crate::db::JournalStore) -> Result<Vec<SyncItem>> {
    sqlx::query_as::<_, SyncItem>(
        "WITH latest AS (SELECT j.id AS job_id,j.environment_id AS environment,j.origin_subject,j.mode,j.cancel_requested, \
         i.ordinal,i.agent,i.native_id,i.title,i.workdir,i.status,i.reason,i.error_code,i.source_revision, \
         ROW_NUMBER() OVER(PARTITION BY j.environment_id,i.agent,i.native_id ORDER BY j.rowid DESC) AS priority \
         FROM import_jobs j JOIN import_items i ON i.job_id=j.id \
         WHERE j.scan_complete=1 AND j.status IN ('completed','failed','cancelled')) \
         SELECT * FROM latest WHERE priority=1 AND mode='import_and_watch' AND cancel_requested=0 \
         AND status IN ('imported','refreshed','failed') \
         AND NOT EXISTS(SELECT 1 FROM import_jobs active WHERE active.environment_id=latest.environment AND active.status IN ('queued','running'))",
    )
    .fetch_all(store.raw_pool())
    .await
    .map_err(Error::Database)
}

/// Refresh changed members of the latest explicit import snapshot.
pub async fn sync_once(state: &AppState) -> Result<usize> {
    let candidates = sync_candidates(&state.store).await?;
    let mut groups = BTreeMap::<(String, String), Vec<SyncItem>>::new();
    let mut permissions = BTreeMap::new();
    for candidate in candidates {
        let key = (
            candidate.environment.clone(),
            candidate.origin_subject.clone(),
        );
        let permitted = if let Some(permitted) = permissions.get(&key) {
            *permitted
        } else {
            let permitted = state
                .store
                .has_capability(&key.1, &key.0, "control")
                .await?;
            permissions.insert(key, permitted);
            permitted
        };
        if permitted {
            groups
                .entry((candidate.environment.clone(), candidate.item.agent.clone()))
                .or_default()
                .push(candidate);
        }
    }
    let mut refreshed = 0;
    for ((environment, agent), items) in groups {
        let environment = EnvironmentId::parse(environment)
            .map_err(|error| Error::Internal(error.to_string()))?;
        if !state.env_bridge.environment_connected(&environment).await {
            continue;
        }
        let summaries = match enumerate_native(state, &environment, &agent, None).await {
            Ok(summaries) => summaries,
            Err(error) => {
                for item in items {
                    finish_sync_item(state, &item, None, Err(&error)).await?;
                }
                continue;
            }
        };
        let mut revisions: BTreeMap<_, _> = summaries
            .into_iter()
            .map(|summary| (summary.native_id.to_string(), summary.source_revision))
            .collect();
        let mut changed = Vec::new();
        for item in items {
            let Some(revision) = revisions.remove(&item.item.native_id) else {
                let error = Error::NativeSessionNotFound(item.item.native_id.clone());
                finish_sync_item(state, &item, None, Err(&error)).await?;
                continue;
            };
            if revision.is_some()
                && revision == item.item.source_revision
                && item.item.status != "failed"
            {
                continue;
            }
            changed.push((item, revision));
        }
        let mut pending = stream::iter(changed)
            .map(|(item, revision)| {
                let environment = environment.clone();
                async move {
                    let session_id = SessionId::new(
                        environment.clone(),
                        AgentName::parse(item.item.agent.clone())
                            .expect("persisted agent identity"),
                        sylph_protocol::NativeId::new(&item.item.native_id),
                    );
                    let result = reread_complete(
                        state,
                        &SubjectId(item.origin_subject.clone()),
                        &session_id,
                    )
                    .await;
                    match result {
                        Ok(()) => {
                            finish_sync_item(state, &item, revision.as_deref(), Ok(())).await?;
                            Ok::<usize, Error>(1)
                        }
                        Err(error) => {
                            finish_sync_item(state, &item, None, Err(&error)).await?;
                            Ok(0)
                        }
                    }
                }
            })
            .buffer_unordered(HISTORY_CONCURRENCY);
        while let Some(result) = pending.next().await {
            refreshed += result?;
        }
    }
    Ok(refreshed)
}

async fn reread_complete(
    state: &AppState,
    subject: &SubjectId,
    session_id: &SessionId,
) -> Result<()> {
    let response = super::sessions::reread(
        State(state.clone()),
        Extension(subject.clone()),
        Path(session_id.encode()),
    )
    .await?;
    if response.reread.read_errors > 0 {
        return Err(Error::LinkFailed(format!(
            "Native history contains {} unreadable records; the source revision remains pending for retry",
            response.reread.read_errors,
        )));
    }
    Ok(())
}

async fn finish_sync_item(
    state: &AppState,
    item: &SyncItem,
    revision: Option<&str>,
    result: std::result::Result<(), &Error>,
) -> Result<()> {
    let (status, reason, code) = match result {
        Ok(()) => ("refreshed", None, None),
        Err(error) => {
            let response = error.to_response();
            (
                "failed",
                Some(response.error.message),
                Some(response.error.code),
            )
        }
    };
    let mut tx = state
        .store
        .raw_pool()
        .begin()
        .await
        .map_err(Error::Database)?;
    sqlx::query("UPDATE import_items SET status=?,reason=?,error_code=?,source_revision=CASE WHEN ?='refreshed' THEN ? ELSE source_revision END WHERE job_id=? AND ordinal=?")
        .bind(status).bind(reason).bind(code).bind(status).bind(revision)
        .bind(&item.job_id).bind(item.item.ordinal).execute(&mut *tx).await.map_err(Error::Database)?;
    sqlx::query("UPDATE import_jobs SET updated_at_ms=? WHERE id=?")
        .bind(now_ms())
        .bind(&item.job_id)
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
    tx.commit().await.map_err(Error::Database)
}

async fn enumerate_native(
    state: &AppState,
    environment: &EnvironmentId,
    agent: &str,
    workdir: Option<String>,
) -> Result<Vec<NativeSummary>> {
    let command = Command::new(
        CommandRequestId::parse(format!("import-scan-{}", uuid::Uuid::new_v4().simple()))
            .map_err(|error| Error::Internal(error.to_string()))?,
        IdempotencyPolicy::Replayable,
        EnvironmentCommand::Enumerate {
            agent: agent.into(),
            workdir,
        },
    );
    let outcome = state
        .env_bridge
        .dispatch_and_wait(environment, command, Duration::from_secs(60))
        .await
        .map_err(|error| Error::LinkFailed(error.to_string()))?;
    let value = match outcome {
        CommandOutcome::Accepted {
            result: Some(value),
        } => value,
        CommandOutcome::Rejected { code, message } => {
            return Err(Error::LinkFailed(format!("{code}: {message}")))
        }
        _ => {
            return Err(Error::LinkFailed(
                "Native enumeration did not return a snapshot".into(),
            ))
        }
    };
    serde_json::from_value(value)
        .map_err(|error| Error::Internal(format!("Malformed native enumeration: {error}")))
}

fn spawn(state: AppState, id: String) {
    tokio::spawn(async move {
        let mut next = Some(id);
        while let Some(id) = next {
            if let Err(error) = run(&state, &id).await {
                tracing::error!(job = %id, %error, "native history import failed");
                if let Err(error) = sqlx::query("UPDATE import_jobs SET status='failed',error=?,updated_at_ms=? WHERE id=? AND status='running'")
                    .bind(error.to_response().error.message).bind(now_ms()).bind(&id).execute(state.store.raw_pool()).await {
                    tracing::error!(job = %id, %error, "could not persist failed import");
                }
            }
            let Ok(current) = job(&state, &id).await else {
                break;
            };
            if current.active() {
                break;
            }
            next = sqlx::query_scalar("SELECT id FROM import_jobs WHERE environment_id=? AND status='queued' ORDER BY rowid LIMIT 1")
                .bind(current.environment).fetch_optional(state.store.raw_pool()).await.ok().flatten();
        }
    });
}

async fn cancelled(state: &AppState, id: &str) -> Result<bool> {
    let current = job(state, id).await?;
    if !current.cancel_requested {
        return Ok(false);
    }
    sqlx::query("UPDATE import_jobs SET status='cancelled',updated_at_ms=? WHERE id=?")
        .bind(now_ms())
        .bind(id)
        .execute(state.store.raw_pool())
        .await
        .map_err(Error::Database)?;
    Ok(true)
}

async fn run(state: &AppState, id: &str) -> Result<()> {
    let claimed = sqlx::query(
        "UPDATE import_jobs SET status='running',updated_at_ms=? WHERE id=? AND status='queued' \
         AND NOT EXISTS(SELECT 1 FROM import_jobs active WHERE active.environment_id=import_jobs.environment_id AND active.status='running')",
    )
    .bind(now_ms())
    .bind(id)
    .execute(state.store.raw_pool())
    .await
    .map_err(Error::Database)?
    .rows_affected()
        > 0;
    if !claimed {
        return Ok(());
    }
    let current = job(state, id).await?;
    let subject = SubjectId(current.origin_subject.clone());
    require_capability(state, &subject, &current.environment, "control").await?;
    let request_json: String =
        sqlx::query_scalar("SELECT request_json FROM import_jobs WHERE id=?")
            .bind(id)
            .fetch_one(state.store.raw_pool())
            .await
            .map_err(Error::Database)?;
    let request: CreateImportRequest =
        serde_json::from_str(&request_json).map_err(|e| Error::Internal(e.to_string()))?;
    if !current.scan_complete {
        discover(state, &current, &request).await?;
    }
    loop {
        if cancelled(state, id).await? {
            return Ok(());
        }
        require_capability(state, &subject, &current.environment, "control").await?;
        // A native pass is independent per session. Processing a small batch in
        // parallel keeps a large workspace from taking one full adapter round trip
        // per row while staying below the envagent's command capacity and preserving
        // durable item-level progress/cancellation.
        let items = sqlx::query_as::<_, ImportItem>("SELECT ordinal,agent,native_id,title,workdir,status,reason,error_code,source_revision FROM import_items WHERE job_id=? AND status='pending' ORDER BY ordinal LIMIT ?")
            .bind(id).bind(HISTORY_CONCURRENCY as i64).fetch_all(state.store.raw_pool()).await.map_err(Error::Database)?;
        if items.is_empty() {
            break;
        }
        for item in &items {
            set_item(state, id, item, "running", None, None).await?;
        }
        // Persist inside each polled future. An outer SQL write can wait on a
        // transaction held by another future that buffer_unordered stops polling.
        let mut results = stream::iter(items.into_iter())
            .map(|item| {
                let state = state.clone();
                let current = current.clone();
                let subject = subject.clone();
                async move {
                    let result = import_item(&state, &current, &subject, &item).await;
                    match result {
                        Ok(status) => set_item(&state, id, &item, status, None, None).await,
                        Err(error) => {
                            let status = if matches!(
                                error,
                                Error::NativeSessionNotResumable | Error::Forbidden(_)
                            ) {
                                "skipped"
                            } else {
                                "failed"
                            };
                            let response = error.to_response();
                            set_item(
                                &state,
                                id,
                                &item,
                                status,
                                Some(&response.error.message),
                                Some(&response.error.code),
                            )
                            .await
                        }
                    }
                }
            })
            .buffer_unordered(HISTORY_CONCURRENCY);
        while let Some(result) = results.next().await {
            result?;
        }
    }
    sqlx::query("UPDATE import_jobs SET status=CASE WHEN cancel_requested=1 THEN 'cancelled' ELSE 'completed' END,updated_at_ms=? WHERE id=?")
        .bind(now_ms()).bind(id).execute(state.store.raw_pool()).await.map_err(Error::Database)?;
    Ok(())
}

async fn set_item(
    state: &AppState,
    id: &str,
    item: &ImportItem,
    status: &str,
    reason: Option<&str>,
    code: Option<&str>,
) -> Result<()> {
    let mut tx = state
        .store
        .raw_pool()
        .begin()
        .await
        .map_err(Error::Database)?;
    sqlx::query(
        "UPDATE import_items SET status=?,reason=?,error_code=? WHERE job_id=? AND ordinal=?",
    )
    .bind(status)
    .bind(reason)
    .bind(code)
    .bind(id)
    .bind(item.ordinal)
    .execute(&mut *tx)
    .await
    .map_err(Error::Database)?;
    sqlx::query("UPDATE import_jobs SET updated_at_ms=? WHERE id=?")
        .bind(now_ms())
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
    tx.commit().await.map_err(Error::Database)
}

async fn discover(
    state: &AppState,
    current: &ImportJob,
    request: &CreateImportRequest,
) -> Result<()> {
    let environment = EnvironmentId::parse(current.environment.clone())
        .map_err(|e| Error::InvalidRequest(e.to_string()))?;
    let inventory = state
        .env_bridge
        .list_environments()
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
        .into_iter()
        .find(|e| e.id == environment)
        .ok_or_else(|| Error::EnvironmentUnreachable(current.environment.clone()))?;
    let agents = request.agents.clone().unwrap_or_else(|| {
        inventory
            .agents
            .iter()
            .filter(|a| a.available)
            .map(|a| a.name.clone())
            .collect()
    });
    for agent in agents {
        if cancelled(state, &current.id).await? {
            return Ok(());
        }
        if !inventory
            .agents
            .iter()
            .any(|a| a.name == agent && a.available)
        {
            return Err(Error::InvalidRequest(format!(
                "Agent '{agent}' is unavailable on this environment"
            )));
        }
        let summaries =
            enumerate_native(state, &environment, &agent, request.workdir.clone()).await?;
        // Persist the snapshot before importing; paging a mutable vendor listing by
        // offset can silently skip sessions that gain activity between pages.
        let mut tx = state
            .store
            .raw_pool()
            .begin()
            .await
            .map_err(Error::Database)?;
        let mut ordinal: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(ordinal),-1)+1 FROM import_items WHERE job_id=?",
        )
        .bind(&current.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::Database)?;
        for summary in summaries {
            let missing = summary.workdir.as_ref().is_none_or(|p| p.trim().is_empty());
            let added = sqlx::query("INSERT INTO import_items(job_id,ordinal,agent,native_id,title,workdir,status,reason,error_code,source_revision) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(job_id,agent,native_id) DO NOTHING")
                .bind(&current.id).bind(ordinal).bind(&agent).bind(summary.native_id.as_str()).bind(summary.title).bind(summary.workdir)
                .bind(if missing { "skipped" } else { "pending" })
                .bind(missing.then_some("Native session recorded no working directory"))
                .bind(missing.then_some("NATIVE_SESSION_NOT_RESUMABLE"))
                .bind(summary.source_revision)
                .execute(&mut *tx).await.map_err(Error::Database)?.rows_affected();
            ordinal += added as i64;
        }
        tx.commit().await.map_err(Error::Database)?;
    }
    sqlx::query("UPDATE import_jobs SET scan_complete=1,updated_at_ms=? WHERE id=?")
        .bind(now_ms())
        .bind(&current.id)
        .execute(state.store.raw_pool())
        .await
        .map_err(Error::Database)?;
    Ok(())
}

async fn import_item(
    state: &AppState,
    current: &ImportJob,
    subject: &SubjectId,
    item: &ImportItem,
) -> Result<&'static str> {
    let session_id = SessionId::new(
        EnvironmentId::parse(current.environment.clone())
            .map_err(|e| Error::InvalidRequest(e.to_string()))?,
        AgentName::parse(item.agent.clone()).map_err(|e| Error::InvalidRequest(e.to_string()))?,
        sylph_protocol::NativeId::new(&item.native_id),
    );
    if state.store.get_session(&session_id).await?.is_some() {
        if let Some(revision) = item.source_revision.as_deref() {
            let previous: Option<(String, Option<String>)> = sqlx::query_as(
                "SELECT i.status,i.source_revision FROM import_items i \
                 JOIN import_jobs j ON j.id=i.job_id WHERE j.environment_id=? \
                 AND i.agent=? AND i.native_id=? AND j.id<>? \
                 ORDER BY j.rowid DESC LIMIT 1",
            )
            .bind(&current.environment)
            .bind(&item.agent)
            .bind(&item.native_id)
            .bind(&current.id)
            .fetch_optional(state.store.raw_pool())
            .await
            .map_err(Error::Database)?;
            if previous.is_some_and(|(status, previous)| {
                matches!(status.as_str(), "imported" | "refreshed")
                    && previous.as_deref() == Some(revision)
            }) {
                return Ok("refreshed");
            }
        }
        reread_complete(state, subject, &session_id).await?;
        return Ok("refreshed");
    }
    let result = super::sessions::adopt(
        State(state.clone()),
        Extension(subject.clone()),
        Json(AdoptSessionRequest {
            environment: current.environment.clone(),
            agent: item.agent.clone(),
            native_id: item.native_id.clone(),
            watch: false,
            inject_identity: false,
            title: None,
        }),
    )
    .await;
    match result {
        Ok(_) => Ok("imported"),
        Err(Error::SessionAlreadyPresent) => {
            reread_complete(state, subject, &session_id).await?;
            Ok("refreshed")
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_snapshot(store: &crate::db::JournalStore, id: &str, mode: &str, cancelled: bool) {
        sqlx::query("INSERT INTO import_jobs(id,environment_id,origin_subject,mode,idempotency_key,request_json,status,scan_complete,cancel_requested,created_at_ms,updated_at_ms) VALUES(?,'env7fk2','subject',?,?,'{}',?,1,?,1,1)")
            .bind(id).bind(mode).bind(id).bind(if cancelled { "cancelled" } else { "completed" })
            .bind(cancelled).execute(store.raw_pool()).await.unwrap();
        sqlx::query("INSERT INTO import_items(job_id,ordinal,agent,native_id,status,source_revision) VALUES(?,0,'codex','native','imported','revision')")
            .bind(id).execute(store.raw_pool()).await.unwrap();
    }

    #[tokio::test]
    async fn equal_timestamp_imports_follow_transaction_insertion_order() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::db::JournalStore::new(root.path().join("journal.db").to_str().unwrap())
            .await
            .unwrap();
        seed_snapshot(&store, "z-earlier", "import_and_watch", false).await;
        seed_snapshot(&store, "a-later", "import_once", false).await;
        assert!(sync_candidates(&store).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelling_the_latest_subscription_does_not_restore_an_older_one() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::db::JournalStore::new(root.path().join("journal.db").to_str().unwrap())
            .await
            .unwrap();
        seed_snapshot(&store, "first", "import_and_watch", false).await;
        seed_snapshot(&store, "cancelled", "import_and_watch", true).await;
        assert!(sync_candidates(&store).await.unwrap().is_empty());
    }
}
