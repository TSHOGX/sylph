//! A separately checkpointed FTS index with immutable bounded result jobs.
//!
//! Search snapshots copy only bounded hits and release SQLite before returning pages.
//! Their trace lease and payload pins keep the corresponding membership/body revisions
//! readable; the current FTS index is never used as a cursor over an earlier query.

use super::generation::Snapshot;
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
    Row, SqlitePool,
};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use sylph_protocol::trace::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchLimits {
    pub max_concurrent_queries: usize,
    pub max_documents_per_batch: usize,
    pub max_document_bytes: usize,
    pub max_document_metadata_bytes: usize,
    pub max_snippet_chars: usize,
    pub max_batch_bytes: usize,
    pub max_query_bytes: usize,
    pub max_results: usize,
    pub max_result_bytes: usize,
    pub max_vm_steps: u64,
    pub max_jobs: usize,
    pub job_lifetime_ms: i64,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            max_concurrent_queries: 4,
            max_documents_per_batch: 128,
            max_document_bytes: 16 * 1024,
            max_document_metadata_bytes: 16 * 1024,
            max_snippet_chars: 256,
            max_batch_bytes: 1024 * 1024,
            max_query_bytes: 4096,
            max_results: 256,
            max_result_bytes: 256 * 1024,
            max_vm_steps: 1_000_000,
            max_jobs: 64,
            job_lifetime_ms: 120_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
    #[error("search index has not reached the requested trace revision")]
    Indexing,
    #[error("search limits exceeded: {0}")]
    Limit(&'static str),
    #[error("search result job has expired or was invalidated")]
    Expired,
    #[error("search job belongs to another subject")]
    SubjectMismatch,
    #[error("search cursor does not belong to the query")]
    InvalidCursor,
    #[error("invalid search update: {0}")]
    Invalid(String),
    #[error("invalid search query: {0}")]
    InvalidQuery(String),
    #[error("search work budget exhausted")]
    WorkBudget,
}
type Result<T> = std::result::Result<T, SearchError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchHistoryMode {
    Authored,
    SelectedContext,
    RetainedArchive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchDocument {
    pub key: String,
    pub agent: AgentRef,
    pub original_owner: AgentRef,
    pub anchor: AnyObjectRef,
    pub object: AnyObjectRef,
    pub selection: Option<SelectionRef>,
    pub mode: SearchHistoryMode,
    pub payload: PayloadRef,
    pub representation: Representation,
    pub body_revision: String,
    pub byte_start: u64,
    pub text: String,
    pub ingestion_epoch: IngestionEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub agent: AgentRef,
    pub original_owner: AgentRef,
    pub anchor: AnyObjectRef,
    pub object: AnyObjectRef,
    pub selection: Option<SelectionRef>,
    pub payload: PayloadRef,
    pub representation: Representation,
    pub body_revision: String,
    pub byte_start: u64,
    pub snippet: String,
    pub ingestion_epoch: IngestionEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchScope {
    pub agent: Option<AgentRef>,
    pub selection: Option<SelectionRef>,
    pub mode: SearchHistoryMode,
    pub representation: Option<Representation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub job_id: String,
    pub view: Snapshot,
    pub trace_lease: String,
    pub hits: Vec<SearchHit>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub result_limit_reached: bool,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchCursor {
    job_id: String,
    subject: String,
    offset: usize,
    scope_hash: String,
}

#[derive(Clone, Copy)]
pub struct SearchQueryContext<'a> {
    pub subject: &'a str,
    pub query: &'a str,
    pub scope: &'a SearchScope,
    pub allowed_environments: &'a BTreeSet<String>,
}
impl SearchQueryContext<'_> {
    fn scope_hash(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(&(
            self.query,
            self.scope,
            self.allowed_environments,
        ))?)
        .to_hex()
        .to_string())
    }
}

#[cfg(test)]
type QueryPause =
    Arc<tokio::sync::Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>>;

#[derive(Clone)]
pub struct SearchStore {
    writer: SqlitePool,
    readers: SqlitePool,
    generation: String,
    limits: SearchLimits,
    #[cfg(test)]
    before_query: QueryPause,
}

impl SearchStore {
    pub async fn open(path: &Path, generation: String, limits: SearchLimits) -> Result<Self> {
        if [
            limits.max_concurrent_queries,
            limits.max_documents_per_batch,
            limits.max_document_bytes,
            limits.max_document_metadata_bytes,
            limits.max_snippet_chars,
            limits.max_batch_bytes,
            limits.max_query_bytes,
            limits.max_results,
            limits.max_result_bytes,
            limits.max_jobs,
        ]
        .contains(&0)
            || limits.max_vm_steps == 0
            || limits.job_lifetime_ms <= 0
        {
            return Err(SearchError::Invalid("positive limits are required".into()));
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true);
        let writer = crate::db::sqlite::pool(options.clone(), 1, false, true).await?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&writer)
            .await?;
        if version == 0 {
            let mut tx = writer.begin_with("BEGIN IMMEDIATE").await?;
            sqlx::raw_sql(include_str!("search/schema.sql"))
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO search_state VALUES(1,?,0,0,0)")
                .bind(&generation)
                .execute(&mut *tx)
                .await?;
            sqlx::query("PRAGMA user_version=1")
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        } else if version != 1 {
            return Err(SearchError::Invalid("unsupported search schema".into()));
        }
        let stored: String =
            sqlx::query_scalar("SELECT generation FROM search_state WHERE singleton=1")
                .fetch_one(&writer)
                .await?;
        if stored != generation {
            return Err(SearchError::Invalid(
                "search database belongs to another generation".into(),
            ));
        }
        let readers =
            crate::db::sqlite::pool(options, limits.max_concurrent_queries as u32, true, true)
                .await?;
        Ok(Self {
            writer,
            readers,
            generation,
            limits,
            #[cfg(test)]
            before_query: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub fn limits(&self) -> &SearchLimits {
        &self.limits
    }

    pub async fn checkpoint(&self) -> Result<Option<Snapshot>> {
        let row = sqlx::query(
            "SELECT evidence_cut,view_revision,ready FROM search_state WHERE singleton=1",
        )
        .fetch_one(&self.readers)
        .await?;
        if row.get::<i64, _>("ready") == 0 {
            return Ok(None);
        }
        Ok(Some(Snapshot {
            generation: self.generation.clone(),
            evidence_cut: row.get::<i64, _>("evidence_cut") as u64,
            view_revision: row.get::<i64, _>("view_revision") as u64,
        }))
    }

    /// Complete projection units update document membership and index cut together.
    pub async fn apply(
        &self,
        expected_revision: u64,
        snapshot: &Snapshot,
        documents: &[SearchDocument],
        removed: &[String],
        complete: bool,
    ) -> Result<()> {
        if snapshot.generation != self.generation
            || snapshot.view_revision < expected_revision
            || documents.len() + removed.len() > self.limits.max_documents_per_batch
        {
            return Err(SearchError::Invalid(
                "index batch scope, order or count".into(),
            ));
        }
        // Count the complete submitted batch, including text and removal keys.
        // Metadata-only accounting hid most FTS work behind a plausible small size.
        let bytes = serde_json::to_vec(&(documents, removed))?.len();
        if bytes > self.limits.max_batch_bytes {
            return Err(SearchError::Limit("index batch bytes"));
        }
        let mut prepared = Vec::new();
        for document in documents {
            if document.text.len() > self.limits.max_document_bytes
                || document.key.len() > self.limits.max_query_bytes
            {
                return Err(SearchError::Limit("document bytes"));
            }
            let json = serde_json::to_string(&SearchHit {
                agent: document.agent.clone(),
                original_owner: document.original_owner.clone(),
                anchor: document.anchor.clone(),
                object: document.object.clone(),
                selection: document.selection.clone(),
                payload: document.payload.clone(),
                representation: document.representation,
                body_revision: document.body_revision.clone(),
                byte_start: document.byte_start,
                snippet: String::new(),
                ingestion_epoch: document.ingestion_epoch.clone(),
            })?;
            if json.len() > self.limits.max_document_metadata_bytes {
                return Err(SearchError::Limit("document metadata bytes"));
            }
            prepared.push((document, json));
        }
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let row =
            sqlx::query("SELECT view_revision,evidence_cut FROM search_state WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await?;
        if row.get::<i64, _>("view_revision") as u64 != expected_revision
            || snapshot.evidence_cut < row.get::<i64, _>("evidence_cut") as u64
        {
            return Err(SearchError::Indexing);
        }
        for key in removed {
            if let Some(rowid) = sqlx::query_scalar::<_, i64>(
                "DELETE FROM search_documents WHERE document_key=? RETURNING id",
            )
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?
            {
                sqlx::query("DELETE FROM search_fts WHERE rowid=?")
                    .bind(rowid)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        for (document, json) in prepared {
            let id:i64=sqlx::query_scalar("INSERT INTO search_documents(document_key,environment,agent_ref,selection_ref,history_mode,representation,document_json) VALUES(?,?,?,?,?,?,?) ON CONFLICT(document_key) DO UPDATE SET environment=excluded.environment,agent_ref=excluded.agent_ref,selection_ref=excluded.selection_ref,history_mode=excluded.history_mode,representation=excluded.representation,document_json=excluded.document_json RETURNING id")
                .bind(&document.key).bind(document.agent.environment().as_str()).bind(document.agent.encode()).bind(document.selection.as_ref().map(SelectionRef::encode)).bind(serde_json::to_string(&document.mode)?).bind(serde_json::to_string(&document.representation)?).bind(json).fetch_one(&mut *tx).await?;
            sqlx::query("DELETE FROM search_fts WHERE rowid=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO search_fts(rowid,text) VALUES(?,?)")
                .bind(id)
                .bind(&document.text)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "UPDATE search_state SET evidence_cut=?,view_revision=?,ready=? WHERE singleton=1",
        )
        .bind(snapshot.evidence_cut as i64)
        .bind(snapshot.view_revision as i64)
        .bind(i64::from(complete))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// `allowed_environments` is freshly evaluated by the caller for this Subject.
    /// The supplied trace lease must pin exactly this index checkpoint before querying.
    pub async fn start_query(
        &self,
        context: SearchQueryContext<'_>,
        snapshot: &Snapshot,
        trace_lease: &str,
        now_ms: i64,
        page_size: usize,
    ) -> Result<SearchPage> {
        let SearchQueryContext {
            subject,
            query,
            scope,
            allowed_environments,
        } = context;
        if query.is_empty()
            || query.len() > self.limits.max_query_bytes
            || page_size == 0
            || page_size > self.limits.max_results
        {
            return Err(SearchError::Limit("query input"));
        }
        if self.checkpoint().await?.as_ref() != Some(snapshot) {
            return Err(SearchError::Indexing);
        }
        #[cfg(test)]
        if let Some((entered, resume)) = self.before_query.lock().await.take() {
            entered.notify_one();
            resume.notified().await;
        }
        let mut connection = self.readers.acquire().await?;
        let steps = Arc::new(AtomicU64::new(0));
        let counter = steps.clone();
        let budget = self.limits.max_vm_steps;
        connection
            .lock_handle()
            .await?
            .set_progress_handler(1, move || counter.fetch_add(1, Ordering::Relaxed) < budget);
        let query_result = async {
            let mut tx = sqlx::Acquire::begin(&mut *connection).await?;
            let current: Option<(i64,i64)> = sqlx::query_as("SELECT view_revision,evidence_cut FROM search_state WHERE singleton=1 AND ready=1").fetch_optional(&mut *tx).await?;
            if current != Some((snapshot.view_revision as i64,snapshot.evidence_cut as i64)) { return Err(SearchError::Indexing); }
            // Candidates contain sizes and bounded excerpts only. Hydrate full
            // metadata under the same SQLite snapshot after the result byte gate.
            let rows = sqlx::query("SELECT d.id,length(CAST(d.document_json AS BLOB)) AS metadata_bytes,substr(snippet(search_fts,0,'','',' … ',24),1,?) AS excerpt FROM search_fts JOIN search_documents d ON d.id=search_fts.rowid WHERE search_fts MATCH ? AND d.environment IN (SELECT value FROM json_each(?)) AND (? IS NULL OR d.agent_ref=?) AND (? IS NULL OR d.selection_ref=?) AND d.history_mode=? AND (? IS NULL OR d.representation=?) ORDER BY bm25(search_fts),d.id LIMIT ?")
                .bind(self.limits.max_snippet_chars as i64).bind(query).bind(serde_json::to_string(allowed_environments)?).bind(scope.agent.as_ref().map(AgentRef::encode)).bind(scope.agent.as_ref().map(AgentRef::encode)).bind(scope.selection.as_ref().map(SelectionRef::encode)).bind(scope.selection.as_ref().map(SelectionRef::encode)).bind(serde_json::to_string(&scope.mode)?).bind(scope.representation.map(|v|serde_json::to_string(&v).unwrap())).bind(scope.representation.map(|v|serde_json::to_string(&v).unwrap())).bind((self.limits.max_results+1) as i64)
                .fetch_all(&mut *tx).await.map_err(|error| search_sql_error(error, steps.load(Ordering::Relaxed)>=budget, query))?;
            let mut limited = rows.len() > self.limits.max_results;
            let mut hits = Vec::new();
            let mut returned_bytes = 0usize;
            for row in rows.into_iter().take(self.limits.max_results) {
                let snippet: String = row.get("excerpt");
                let metadata = usize::try_from(row.get::<i64,_>("metadata_bytes")).map_err(|_| SearchError::Limit("document metadata bytes"))?;
                let excerpt_bytes = serde_json::to_vec(&snippet)?.len().saturating_sub(2);
                let bytes = metadata.checked_add(excerpt_bytes).ok_or(SearchError::Limit("result bytes"))?;
                if metadata > self.limits.max_document_metadata_bytes || bytes > self.limits.max_result_bytes.saturating_sub(returned_bytes) {
                    limited = true; break;
                }
                let json: String = sqlx::query_scalar("SELECT document_json FROM search_documents WHERE id=?").bind(row.get::<i64,_>("id")).fetch_one(&mut *tx).await?;
                let mut hit: SearchHit = serde_json::from_str(&json)?;
                hit.snippet = snippet;
                returned_bytes += bytes;
                hits.push(hit);
            }
            tx.commit().await?;
            Ok::<_,SearchError>((hits,limited))
        }.await;
        connection.lock_handle().await?.remove_progress_handler();
        drop(connection);
        let (hits, limited) = query_result.map_err(|error| match error {
            SearchError::Database(database) => {
                search_sql_error(database, steps.load(Ordering::Relaxed) >= budget, query)
            }
            other => other,
        })?;
        let id = uuid::Uuid::new_v4().to_string();
        let scope_hash = context.scope_hash()?;
        let expires = now_ms.saturating_add(self.limits.job_lifetime_ms);
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM search_jobs WHERE expires_at_ms<=?")
            .bind(now_ms)
            .execute(&mut *tx)
            .await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM search_jobs")
            .fetch_one(&mut *tx)
            .await?;
        if count >= self.limits.max_jobs as i64 {
            return Err(SearchError::Limit("result jobs"));
        }
        sqlx::query("INSERT INTO search_jobs VALUES(?,?,?,?,?,?,?,?)")
            .bind(&id)
            .bind(subject)
            .bind(&scope_hash)
            .bind(serde_json::to_string(snapshot)?)
            .bind(trace_lease)
            .bind(expires)
            .bind(i64::from(limited))
            .bind(serde_json::to_string(&hits)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.page(subject, &id, None, now_ms, page_size).await
    }

    pub async fn page(
        &self,
        subject: &str,
        job_id: &str,
        cursor: Option<&str>,
        now_ms: i64,
        page_size: usize,
    ) -> Result<SearchPage> {
        use base64::Engine;
        if page_size == 0 || page_size > self.limits.max_results {
            return Err(SearchError::Limit("result page"));
        }
        let row = sqlx::query("SELECT * FROM search_jobs WHERE id=? AND expires_at_ms>?")
            .bind(job_id)
            .bind(now_ms)
            .fetch_optional(&self.readers)
            .await?
            .ok_or(SearchError::Expired)?;
        if row.get::<String, _>("subject") != subject {
            return Err(SearchError::SubjectMismatch);
        }
        let scope_hash: String = row.get("scope_hash");
        let offset = match cursor {
            None => 0,
            Some(value) => {
                if value.len() > self.limits.max_query_bytes {
                    return Err(SearchError::InvalidCursor);
                }
                let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(value)
                    .map_err(|_| SearchError::InvalidCursor)?;
                let cursor: SearchCursor =
                    serde_json::from_slice(&bytes).map_err(|_| SearchError::InvalidCursor)?;
                if cursor.job_id != job_id
                    || cursor.subject != subject
                    || cursor.scope_hash != scope_hash
                {
                    return Err(SearchError::InvalidCursor);
                }
                cursor.offset
            }
        };
        let all: Vec<SearchHit> = serde_json::from_str(&row.get::<String, _>("hits_json"))?;
        if offset > all.len() {
            return Err(SearchError::InvalidCursor);
        }
        let end = offset.saturating_add(page_size).min(all.len());
        let has_more = end < all.len();
        let next_cursor = if has_more {
            Some(
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(
                    &SearchCursor {
                        job_id: job_id.into(),
                        subject: subject.into(),
                        offset: end,
                        scope_hash,
                    },
                )?),
            )
        } else {
            None
        };
        Ok(SearchPage {
            job_id: job_id.into(),
            view: serde_json::from_str(&row.get::<String, _>("snapshot_json"))?,
            trace_lease: row.get("trace_lease"),
            hits: all[offset..end].to_vec(),
            next_cursor,
            has_more,
            result_limit_reached: row.get::<i64, _>("limited") != 0,
            expires_at_ms: row.get("expires_at_ms"),
        })
    }

    pub async fn resume_for(
        &self,
        context: SearchQueryContext<'_>,
        cursor: &str,
        now_ms: i64,
        page_size: usize,
    ) -> Result<SearchPage> {
        use base64::Engine;
        if cursor.len() > self.limits.max_query_bytes {
            return Err(SearchError::InvalidCursor);
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| SearchError::InvalidCursor)?;
        let decoded: SearchCursor =
            serde_json::from_slice(&bytes).map_err(|_| SearchError::InvalidCursor)?;
        if decoded.subject != context.subject {
            return Err(SearchError::SubjectMismatch);
        }
        if decoded.scope_hash != context.scope_hash()? {
            return Err(SearchError::InvalidCursor);
        }
        self.page(
            context.subject,
            &decoded.job_id,
            Some(cursor),
            now_ms,
            page_size,
        )
        .await
    }

    pub async fn invalidate(&self) -> Result<()> {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("UPDATE search_state SET ready=0 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM search_jobs")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn begin_reindex(&self) -> Result<()> {
        sqlx::query("UPDATE search_state SET ready=0 WHERE singleton=1")
            .execute(&self.writer)
            .await?;
        loop {
            let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
            let ids: Vec<i64> =
                sqlx::query_scalar("SELECT id FROM search_documents ORDER BY id LIMIT ?")
                    .bind(self.limits.max_documents_per_batch as i64)
                    .fetch_all(&mut *tx)
                    .await?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                sqlx::query("DELETE FROM search_fts WHERE rowid=?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM search_documents WHERE id=?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            tokio::task::yield_now().await;
        }
        Ok(())
    }
    pub async fn revision(&self) -> Result<u64> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT view_revision FROM search_state WHERE singleton=1")
                .fetch_one(&self.readers)
                .await? as u64,
        )
    }
}

fn search_sql_error(error: sqlx::Error, budget_exhausted: bool, query: &str) -> SearchError {
    if budget_exhausted {
        return SearchError::WorkBudget;
    }
    if let sqlx::Error::Database(database) = &error {
        let message = database.message();
        if database.code().as_deref() == Some("1")
            && (message.starts_with("fts5: syntax error")
                || message == "unterminated string"
                || message.starts_with("unknown special query:")
                || message
                    .strip_prefix("no such column: ")
                    .is_some_and(|column| query.contains(&format!("{column}:"))))
        {
            return SearchError::InvalidQuery(message.into());
        }
    }
    SearchError::Database(error)
}

#[cfg(test)]
mod tests;
