//! Temporal materialized reader rows. HTTP pages never traverse domain graphs.

use std::{collections::BTreeSet, future::Future};

use serde::{Deserialize, Serialize};
use sqlx::Row;
use sylph_protocol::trace::*;

use super::generation::{GenerationError, GenerationStore, Snapshot};

const READ_FORWARD_SQL: &str = "SELECT order_key,address,anchor_ref,valid_from,valid_to,present,body_bytes,scope_epoch,search_text,directory_state,needs_input,required_environments_json,dependency_epochs_json FROM read_rows WHERE scope_key=? AND (order_key,address,valid_from)>(?,?,?) ORDER BY order_key,address,valid_from LIMIT ?";
const READ_BACKWARD_SQL: &str = "SELECT order_key,address,anchor_ref,valid_from,valid_to,present,body_bytes,scope_epoch,search_text,directory_state,needs_input,required_environments_json,dependency_epochs_json FROM read_rows WHERE scope_key=? AND (order_key,address,valid_from)<(?,?,?) ORDER BY order_key DESC,address DESC,valid_from DESC LIMIT ?";
const READ_ANCHOR_SQL: &str = "SELECT order_key,address,valid_from,scope_epoch,required_environments_json,dependency_epochs_json,search_text,directory_state,needs_input FROM read_rows WHERE scope_key=? AND anchor_ref=? AND valid_from<=? AND (valid_to IS NULL OR valid_to>?) AND present=1 ORDER BY order_key,address,valid_from LIMIT 1";

const READ_MESSAGE_CONTEXT_SQL: &str = "SELECT anchor_ref,valid_to,scope_epoch,required_environments_json,dependency_epochs_json FROM read_rows INDEXED BY read_message_context WHERE scope_key=? AND CASE WHEN json_valid(body_json) THEN json_extract(body_json,'$.content.messageRef') END=? AND valid_from<=? AND present=1 AND CASE WHEN json_valid(body_json) THEN json_extract(body_json,'$.content.kind') END='message' AND CASE WHEN json_valid(body_json) THEN json_extract(body_json,'$.content.delivery') END IS NULL ORDER BY valid_from DESC LIMIT 1";

pub const DIRECTORY_COLLECTION: &str = "spawn-directory";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryMetadata {
    pub component: AgentRef,
    pub parent: Option<AgentRef>,
    pub end_key: Vec<u8>,
    pub spawn_epochs: BTreeSet<IngestionEpoch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRowChange {
    pub directory: Option<DirectoryMetadata>,
    pub scope: String,
    pub address: String,
    pub anchor: Option<TraceAddress>,
    pub order_key: Vec<u8>,
    pub body: Option<serde_json::Value>,
    pub scope_epoch: IngestionEpoch,
    pub required_environments: BTreeSet<String>,
    pub dependency_epochs: BTreeSet<IngestionEpoch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRow {
    pub address: String,
    pub anchor: Option<TraceAddress>,
    pub body: serde_json::Value,
    pub epoch: IngestionEpoch,
    pub required_environments: BTreeSet<String>,
    pub dependency_epochs: BTreeSet<IngestionEpoch>,
}

/// Private optional-provenance metadata. These rows never enter client pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CopyAuthorizationNode {
    pub next: Option<sylph_trace::reading::CopyNodeKey>,
    pub groups: Vec<Vec<BTreeSet<IngestionEpoch>>>,
}

/// Authorization inputs are stored separately so denied bodies are never hydrated.
#[derive(Debug, Clone)]
pub struct ReadCandidate {
    pub epoch: IngestionEpoch,
    pub required_environments: BTreeSet<String>,
    pub dependency_epochs: BTreeSet<IngestionEpoch>,
}

#[derive(Debug, Clone)]
pub struct ReadRowsPage {
    pub rows: Vec<ReadRow>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub has_before: bool,
    pub has_after: bool,
    pub work_exhausted: bool,
    pub total: Option<u64>,
    /// SQLite VM steps this page's own scans spent, summed over every reader
    /// connection it acquired — the anchor/candidate/body reads and, for an
    /// unfiltered page, the separate `read_counts` lookup. A page read is not one
    /// connection, and the progress handler is removed on release, so each
    /// acquisition contributes before it drops.
    ///
    /// This measures the store's scan, *not* the cost of serving the page: the
    /// `authorize` callback runs a caller's own queries per candidate, which
    /// belong to that caller's accounting. `work_exhausted` reports the same
    /// budget from the candidate side and is not recomputed from this value.
    pub vm_steps: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct ReadWindow<'a> {
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub around: Option<&'a TraceAddress>,
    pub limit: usize,
    pub latest: bool,
}

/// A filter is part of the cursor identity, and runs on bounded scalar metadata.
/// Payload hydration happens only after a candidate matches the entire directory query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFilter {
    pub text: String,
    pub state: String,
    pub allowed_environments: Option<BTreeSet<String>>,
}
impl ReadFilter {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.state.is_empty() && self.allowed_environments.is_none()
    }
    fn identity(&self) -> Result<FilterIdentity, GenerationError> {
        // Grant membership is canonical, but its size must not enlarge every cursor.
        Ok(FilterIdentity {
            text: self.text.clone(),
            state: self.state.clone(),
            grants: self
                .allowed_environments
                .as_ref()
                .map(|environments| {
                    serde_json::to_vec(environments)
                        .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
                })
                .transpose()?,
        })
    }
    fn permits(&self, candidate: &ReadCandidate) -> bool {
        self.allowed_environments
            .as_ref()
            .is_none_or(|allowed| candidate.required_environments.is_subset(allowed))
    }
    fn matches(&self, row: &sqlx::sqlite::SqliteRow) -> bool {
        (self.text.is_empty() || row.get::<String, _>("search_text").contains(&self.text))
            && (self.state.is_empty()
                || row.get::<String, _>("directory_state") == self.state
                || (self.state == "needs_input" && row.get::<i64, _>("needs_input") != 0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FilterIdentity {
    text: String,
    state: String,
    grants: Option<String>,
}
fn directory_metadata(body: Option<&serde_json::Value>) -> (String, String, bool) {
    let Some(row) =
        body.and_then(|body| serde_json::from_value::<AgentDirectoryRow>(body.clone()).ok())
    else {
        return (String::new(), String::new(), false);
    };
    let text = format!(
        "{} {} {} {}",
        row.agent.name.as_deref().unwrap_or(""),
        row.agent.address.agent().anchor_session().native_id,
        row.agent.identity_suffix,
        row.agent.vendor
    )
    .to_lowercase();
    let state = match row.own_execution.state {
        ReadExecutionState::ObservedActive { .. } => "active",
        ReadExecutionState::Terminated {
            outcome: TerminalOutcome::Succeeded,
            ..
        } => "completed",
        ReadExecutionState::Terminated {
            outcome: TerminalOutcome::Failed,
            ..
        } => "failed",
        ReadExecutionState::Terminated {
            outcome: TerminalOutcome::Cancelled,
            ..
        } => "cancelled",
        _ => "unknown",
    };
    (
        text,
        state.into(),
        row.required_input.value.is_some_and(|count| count > 0),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cursor {
    generation: String,
    revision: u64,
    scope: String,
    key: Vec<u8>,
    address: String,
    valid_from: u64,
    backward: bool,
    filter: FilterIdentity,
}

pub(crate) const RESULT_REPRESENTATIONS: [ResultRepresentation; 3] = [
    ResultRepresentation::Runtime,
    ResultRepresentation::CallerResult,
    ResultRepresentation::Conversation,
];

/// Result cursors use a distinct indexed collection for every representation.
/// The payload collection certifies readable bytes without scanning missing results.
pub(crate) fn result_collection(
    reference: &AnyObjectRef,
    representation: ResultRepresentation,
    payload_only: bool,
) -> String {
    let representation = match representation {
        ResultRepresentation::Runtime => "runtime",
        ResultRepresentation::CallerResult => "caller_result",
        ResultRepresentation::Conversation => "conversation",
    };
    format!(
        "object-results:{}:{representation}:{}",
        reference.encode(),
        if payload_only {
            "payload"
        } else {
            "occurrence"
        }
    )
}

pub fn read_scope(agent: &AgentRef, selection: Option<&SelectionRef>, collection: &str) -> String {
    serde_json::to_string(&(collection, agent, selection)).expect("typed read scope")
}

pub fn cursor_after(
    snapshot: &Snapshot,
    scope: &str,
    address: &str,
    ordinal: usize,
    filter: &ReadFilter,
) -> Result<String, GenerationError> {
    use base64::Engine;
    Ok(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Cursor {
            generation: snapshot.generation.clone(),
            revision: snapshot.view_revision,
            scope: scope.into(),
            key: (ordinal as u64 + 1).to_be_bytes().to_vec(),
            address: address.into(),
            valid_from: i64::MAX as u64,
            backward: false,
            filter: filter.identity()?,
        })?),
    )
}

pub(crate) async fn apply_rows(
    conn: &mut sqlx::SqliteConnection,
    rows: &[ReadRowChange],
    revision: u64,
) -> Result<(), GenerationError> {
    let mut changes = std::collections::BTreeMap::<String, i64>::new();
    for row in rows {
        let prior: Option<i64> = sqlx::query_scalar(
            "SELECT present FROM read_rows WHERE scope_key=? AND address=? AND valid_to IS NULL",
        )
        .bind(&row.scope)
        .bind(&row.address)
        .fetch_optional(&mut *conn)
        .await
        .map_err(super::generation::map_query_error)?;
        sqlx::query(
            "UPDATE read_rows SET valid_to=? WHERE scope_key=? AND address=? AND valid_to IS NULL",
        )
        .bind(revision as i64)
        .bind(&row.scope)
        .bind(&row.address)
        .execute(&mut *conn)
        .await
        .map_err(super::generation::map_query_error)?;
        let present = i64::from(row.body.is_some());
        let (search_text, directory_state, needs_input) = directory_metadata(row.body.as_ref());
        let body = serde_json::to_string(&row.body)?;
        sqlx::query("INSERT INTO read_rows(scope_key,address,anchor_ref,order_key,valid_from,valid_to,present,body_json,body_bytes,scope_epoch,search_text,directory_state,needs_input,required_environments_json,dependency_epochs_json,directory_end,directory_epochs_json,directory_parent) VALUES(?,?,?,?,?,NULL,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&row.scope).bind(&row.address).bind(row.anchor.as_ref().map(TraceAddress::as_str)).bind(&row.order_key).bind(revision as i64).bind(present).bind(&body).bind(body.len() as i64).bind(row.scope_epoch.as_str()).bind(search_text).bind(directory_state).bind(needs_input).bind(serde_json::to_string(&row.required_environments)?).bind(serde_json::to_string(&row.dependency_epochs)?).bind(row.directory.as_ref().map(|value| value.end_key.as_slice())).bind(row.directory.as_ref().map(|value| serde_json::to_string(&value.spawn_epochs)).transpose()?).bind(row.directory.as_ref().and_then(|value| value.parent.as_ref()).map(AgentRef::encode)).execute(&mut *conn).await.map_err(super::generation::map_query_error)?;
        *changes.entry(row.scope.clone()).or_default() += present - prior.unwrap_or(0);
    }
    for (scope, delta) in changes {
        let prior: Option<i64> = sqlx::query_scalar(
            "SELECT count FROM read_counts WHERE scope_key=? ORDER BY valid_from DESC LIMIT 1",
        )
        .bind(&scope)
        .fetch_optional(&mut *conn)
        .await
        .map_err(super::generation::map_query_error)?;
        let count = prior
            .unwrap_or(0)
            .checked_add(delta)
            .filter(|count| *count >= 0)
            .ok_or_else(|| GenerationError::Invalid("negative reading count".into()))?;
        sqlx::query("INSERT INTO read_counts VALUES(?,?,?) ON CONFLICT(scope_key,valid_from) DO UPDATE SET count=excluded.count")
            .bind(scope)
            .bind(revision as i64)
            .bind(count)
            .execute(&mut *conn)
            .await
            .map_err(super::generation::map_query_error)?;
    }
    Ok(())
}

impl GenerationStore {
    pub async fn read_candidate(
        &self,
        snapshot: &Snapshot,
        scope: &str,
        anchor: &TraceAddress,
    ) -> Result<ReadCandidate, GenerationError> {
        let (mut conn, _) = self.read_connection(snapshot).await?;
        // This covering index keeps a hidden target's body out of the point lookup.
        let row = sqlx::query("SELECT scope_epoch,required_environments_json,dependency_epochs_json,present FROM read_rows INDEXED BY read_anchor WHERE scope_key=? AND anchor_ref=? AND valid_from<=? ORDER BY valid_from DESC LIMIT 1")
            .bind(scope).bind(anchor.as_str()).bind(snapshot.view_revision as i64)
            .fetch_optional(&mut *conn).await.map_err(super::generation::map_query_error)?
            .ok_or(GenerationError::NotFound)?;
        if row.get::<i64, _>("present") == 0 {
            return Err(GenerationError::NotFound);
        }
        decode_candidate(&row)
    }

    /// Resolve a collapsed stream occurrence from the actual rendered message,
    /// without replaying history or fetching protected body JSON before authorization.
    pub async fn message_context(
        &self,
        snapshot: &Snapshot,
        scope: &str,
        message: &MessageRef,
    ) -> Result<Option<(TraceAddress, ReadCandidate)>, GenerationError> {
        let (mut conn, _) = self.read_connection(snapshot).await?;
        let row = sqlx::query(READ_MESSAGE_CONTEXT_SQL)
            .bind(scope)
            .bind(message.encode())
            .bind(snapshot.view_revision as i64)
            .fetch_optional(&mut *conn)
            .await
            .map_err(super::generation::map_query_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        if row
            .get::<Option<i64>, _>("valid_to")
            .is_some_and(|end| end <= snapshot.view_revision as i64)
        {
            return Ok(None);
        }
        let Some(anchor) = row.get::<Option<String>, _>("anchor_ref") else {
            return Ok(None);
        };
        let anchor = TraceAddress::parse(anchor)
            .map_err(|error| GenerationError::Invalid(error.to_string()))?;
        Ok(Some((anchor, decode_candidate(&row)?)))
    }

    pub async fn read_row(
        &self,
        snapshot: &Snapshot,
        scope: &str,
        address: &str,
    ) -> Result<ReadRow, GenerationError> {
        let (mut conn, _) = self.read_connection(snapshot).await?;
        let row=sqlx::query("SELECT address,anchor_ref,body_json,scope_epoch,present,required_environments_json,dependency_epochs_json FROM read_rows WHERE scope_key=? AND address=? AND valid_from<=? ORDER BY valid_from DESC LIMIT 1")
            .bind(scope).bind(address).bind(snapshot.view_revision as i64).fetch_optional(&mut *conn).await.map_err(super::generation::map_query_error)?.ok_or(GenerationError::NotFound)?;
        if row.get::<i64, _>("present") == 0 {
            return Err(GenerationError::NotFound);
        }
        decode(row)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn read_page(
        &self,
        snapshot: &Snapshot,
        scope: &str,
        before: Option<&str>,
        after: Option<&str>,
        around: Option<&TraceAddress>,
        limit: usize,
        latest: bool,
    ) -> Result<ReadRowsPage, GenerationError> {
        self.read_filtered_page(
            snapshot,
            scope,
            before,
            after,
            around,
            limit,
            latest,
            &ReadFilter::default(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn read_filtered_page(
        &self,
        snapshot: &Snapshot,
        scope: &str,
        before: Option<&str>,
        after: Option<&str>,
        around: Option<&TraceAddress>,
        limit: usize,
        latest: bool,
        filter: &ReadFilter,
    ) -> Result<ReadRowsPage, GenerationError> {
        let mut page = self
            .read_authorized_page(
                snapshot,
                scope,
                ReadWindow {
                    before,
                    after,
                    around,
                    limit,
                    latest,
                },
                filter,
                |_| async { Ok(true) },
            )
            .await?;
        if filter.is_empty() {
            let (mut conn, steps) = self.read_connection(snapshot).await?;
            let count: Option<i64> = sqlx::query_scalar("SELECT count FROM read_counts WHERE scope_key=? AND valid_from<=? ORDER BY valid_from DESC LIMIT 1")
                .bind(scope).bind(snapshot.view_revision as i64).fetch_optional(&mut *conn).await.map_err(super::generation::map_query_error)?;
            page.total = Some(count.unwrap_or(0) as u64);
            // This lookup is a second connection with its own counter, so the
            // page's scan cost accumulates rather than being replaced.
            page.vm_steps += steps.load(std::sync::atomic::Ordering::Relaxed);
        }
        Ok(page)
    }

    pub async fn read_authorized_page<F, Fut>(
        &self,
        snapshot: &Snapshot,
        scope: &str,
        window: ReadWindow<'_>,
        filter: &ReadFilter,
        mut authorize: F,
    ) -> Result<ReadRowsPage, GenerationError>
    where
        F: FnMut(ReadCandidate) -> Fut,
        Fut: Future<Output = Result<bool, GenerationError>>,
    {
        use base64::Engine;
        let ReadWindow {
            before,
            after,
            around,
            limit,
            latest,
        } = window;
        if limit == 0
            || limit > self.limits().max_items
            || usize::from(before.is_some())
                + usize::from(after.is_some())
                + usize::from(around.is_some())
                > 1
        {
            return Err(GenerationError::InvalidCursor);
        }
        let tail = latest && before.is_none() && after.is_none() && around.is_none();
        let backward = before.is_some() || tail;
        let filter_identity = filter.identity()?;
        let cursor = before
            .or(after)
            .map(|value| {
                if value.len() > self.limits().max_key_bytes * 4 {
                    return Err(GenerationError::InvalidCursor);
                }
                let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(value)
                    .map_err(|_| GenerationError::InvalidCursor)?;
                let cursor: Cursor =
                    serde_json::from_slice(&bytes).map_err(|_| GenerationError::InvalidCursor)?;
                if cursor.generation != snapshot.generation
                    || cursor.revision != snapshot.view_revision
                    || cursor.scope != scope
                    || cursor.backward != backward
                    || cursor.filter != filter_identity
                {
                    return Err(GenerationError::InvalidCursor);
                }
                Ok(cursor)
            })
            .transpose()?;
        let (mut conn, steps) = self.read_connection(snapshot).await?;
        let anchor = if let Some(around) = around {
            sqlx::query(READ_ANCHOR_SQL)
                .bind(scope)
                .bind(around.as_str())
                .bind(snapshot.view_revision as i64)
                .bind(snapshot.view_revision as i64)
                .fetch_optional(&mut *conn)
                .await
                .map_err(super::generation::map_query_error)?
        } else {
            None
        };
        if around.is_some() && anchor.is_none() {
            return Err(GenerationError::AnchorOutsideSelection);
        }
        if let Some(anchor) = &anchor {
            let candidate = decode_candidate(anchor)?;
            if !filter.matches(anchor)
                || !filter.permits(&candidate)
                || !authorize(candidate).await?
            {
                return Err(GenerationError::AnchorOutsideSelection);
            }
        }
        let key = cursor
            .as_ref()
            .map(|cursor| cursor.key.clone())
            .or_else(|| anchor.as_ref().map(|row| row.get("order_key")))
            .unwrap_or_else(|| if tail { vec![255; 9] } else { vec![] });
        let address = cursor
            .as_ref()
            .map(|cursor| cursor.address.clone())
            .unwrap_or_default();
        let version = cursor.as_ref().map_or(0, |cursor| cursor.valid_from);
        let max_candidates = self
            .limits()
            .max_candidates
            .checked_sub(usize::from(anchor.is_some()))
            .filter(|remaining| *remaining > 0)
            .ok_or(GenerationError::Limit("reading anchor candidate budget"))?;
        let rows = if backward {
            sqlx::query(READ_BACKWARD_SQL)
                .bind(scope)
                .bind(&key)
                .bind(&address)
                .bind(version as i64)
                .bind((max_candidates + 1) as i64)
                .fetch_all(&mut *conn)
                .await
                .map_err(super::generation::map_query_error)?
        } else {
            sqlx::query(READ_FORWARD_SQL)
                .bind(scope)
                .bind(&key)
                .bind(&address)
                .bind(version as i64)
                .bind((max_candidates + 1) as i64)
                .fetch_all(&mut *conn)
                .await
                .map_err(super::generation::map_query_error)?
        };
        let fetched = rows.len();
        let mut visible = Vec::new();
        let mut count = 0u64;
        for row in rows.into_iter().take(max_candidates) {
            let candidate = decode_candidate(&row)?;
            let permitted = is_current(&row, snapshot)
                && filter.matches(&row)
                && filter.permits(&candidate)
                && authorize(candidate).await?;
            count += u64::from(permitted);
            visible.push((row, permitted));
        }
        // A page may prove a whole-scope count without another scan. Continuations
        // cannot reuse that proof: deletion may have changed since the earlier page.
        let total =
            (cursor.is_none() && around.is_none() && fetched <= max_candidates).then_some(count);
        let mut result = Vec::new();
        let mut bytes = 0usize;
        let mut last = None;
        let mut first = None;
        let mut first_scanned = None;
        let mut more = fetched > max_candidates;
        let mut budget = false;
        for (row, permitted) in visible {
            let valid_from = row.get::<i64, _>("valid_from") as u64;
            let position = Cursor {
                generation: snapshot.generation.clone(),
                revision: snapshot.view_revision,
                scope: scope.into(),
                key: row.get("order_key"),
                address: row.get("address"),
                valid_from,
                backward,
                filter: filter_identity.clone(),
            };
            if first_scanned.is_none() {
                first_scanned = Some(position.clone());
            }
            if permitted {
                let size = row.get::<i64, _>("body_bytes") as usize;
                if bytes + size > self.limits().max_summary_bytes {
                    if result.is_empty() {
                        return Err(GenerationError::Limit("individual reading row"));
                    }
                    more = true;
                    break;
                }
                let body=sqlx::query("SELECT address,anchor_ref,body_json,scope_epoch,required_environments_json,dependency_epochs_json FROM read_rows WHERE scope_key=? AND address=? AND valid_from=?")
                    .bind(scope).bind(&position.address).bind(valid_from as i64).fetch_one(&mut *conn).await.map_err(super::generation::map_query_error)?;
                result.push(decode(body)?);
                bytes += size;
                if first.is_none() {
                    first = Some(position.clone());
                }
            }
            last = Some(position);
            if result.len() == limit {
                more = result.len() < count as usize || fetched > max_candidates;
                break;
            }
        }
        if result.len() < limit && fetched > max_candidates {
            budget = true;
        }
        let encode = |mut cursor: Cursor, direction: bool| -> Result<String, GenerationError> {
            cursor.backward = direction;
            Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&cursor)?))
        };
        let has_opposite = cursor.is_some() || around.is_some();
        let first = first.or(first_scanned);
        let (before_cursor, after_cursor) = if backward {
            result.reverse();
            (
                if more {
                    last.clone()
                        .map(|cursor| encode(cursor, true))
                        .transpose()?
                } else {
                    None
                },
                first.map(|cursor| encode(cursor, false)).transpose()?,
            )
        } else {
            (
                if has_opposite {
                    first.map(|cursor| encode(cursor, true)).transpose()?
                } else {
                    None
                },
                if more {
                    last.map(|cursor| encode(cursor, false)).transpose()?
                } else {
                    None
                },
            )
        };
        Ok(ReadRowsPage {
            rows: result,
            before: before_cursor,
            after: after_cursor,
            has_before: if backward { more } else { has_opposite },
            has_after: if backward { has_opposite } else { more },
            work_exhausted: budget,
            total,
            // Read before `conn` drops: release removes the progress handler.
            vm_steps: steps.load(std::sync::atomic::Ordering::Relaxed),
        })
    }
}

fn is_current(row: &sqlx::sqlite::SqliteRow, snapshot: &Snapshot) -> bool {
    row.get::<i64, _>("present") != 0
        && row.get::<i64, _>("valid_from") as u64 <= snapshot.view_revision
        && row
            .get::<Option<i64>, _>("valid_to")
            .is_none_or(|end| end as u64 > snapshot.view_revision)
}

fn decode_candidate(row: &sqlx::sqlite::SqliteRow) -> Result<ReadCandidate, GenerationError> {
    Ok(ReadCandidate {
        epoch: IngestionEpoch::new(row.try_get::<String, _>("scope_epoch")?)
            .map_err(|error| GenerationError::Invalid(error.to_string()))?,
        required_environments: serde_json::from_str(
            &row.try_get::<String, _>("required_environments_json")?,
        )?,
        dependency_epochs: serde_json::from_str(
            &row.try_get::<String, _>("dependency_epochs_json")?,
        )?,
    })
}

fn decode(row: sqlx::sqlite::SqliteRow) -> Result<ReadRow, GenerationError> {
    let candidate = decode_candidate(&row)?;
    Ok(ReadRow {
        address: row.try_get("address")?,
        anchor: row
            .try_get::<Option<String>, _>("anchor_ref")?
            .map(TraceAddress::parse)
            .transpose()
            .map_err(|error| GenerationError::Invalid(error.to_string()))?,
        body: serde_json::from_str(&row.try_get::<String, _>("body_json")?)?,
        epoch: candidate.epoch,
        required_environments: candidate.required_environments,
        dependency_epochs: candidate.dependency_epochs,
    })
}

#[cfg(test)]
mod tests;

mod directory;
pub use directory::*;
