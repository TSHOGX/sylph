//! Temporal materialized reader rows. HTTP pages never traverse domain graphs.

use serde::{Deserialize, Serialize};
use sqlx::Row;
use sylph_protocol::trace::*;

use super::generation::{GenerationError, GenerationStore, Snapshot};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRowChange {
    pub scope: String,
    pub address: String,
    pub anchor: Option<TraceAddress>,
    pub order_key: Vec<u8>,
    pub body: Option<serde_json::Value>,
    pub scope_epoch: IngestionEpoch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRow {
    pub address: String,
    pub anchor: Option<TraceAddress>,
    pub body: serde_json::Value,
    pub epoch: IngestionEpoch,
}

#[derive(Debug, Clone)]
pub struct ReadRowsPage {
    pub rows: Vec<ReadRow>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub has_before: bool,
    pub has_after: bool,
    pub work_exhausted: bool,
    pub total: u64,
}

/// A filter is part of the cursor identity, and runs on bounded scalar metadata.
/// Payload hydration happens only after a candidate matches the entire directory query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFilter {
    pub text: String,
    pub state: String,
}
impl ReadFilter {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.state.is_empty()
    }
    fn matches(&self, row: &sqlx::sqlite::SqliteRow) -> bool {
        (self.text.is_empty() || row.get::<String, _>("search_text").contains(&self.text))
            && (self.state.is_empty()
                || row.get::<String, _>("directory_state") == self.state
                || (self.state == "needs_input" && row.get::<i64, _>("needs_input") != 0))
    }
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
    filter: ReadFilter,
}

pub fn read_scope(agent: &AgentRef, selection: Option<&SelectionRef>, collection: &str) -> String {
    serde_json::to_string(&(collection, agent, selection)).expect("typed read scope")
}

pub fn cursor_after(
    snapshot: &Snapshot,
    scope: &str,
    address: &str,
    ordinal: usize,
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
            filter: ReadFilter::default(),
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
        sqlx::query("INSERT INTO read_rows(scope_key,address,anchor_ref,order_key,valid_from,valid_to,present,body_json,body_bytes,scope_epoch,search_text,directory_state,needs_input) VALUES(?,?,?,?,?,NULL,?,?,?,?,?,?,?)")
            .bind(&row.scope).bind(&row.address).bind(row.anchor.as_ref().map(TraceAddress::as_str)).bind(&row.order_key).bind(revision as i64).bind(present).bind(&body).bind(body.len() as i64).bind(row.scope_epoch.as_str()).bind(search_text).bind(directory_state).bind(needs_input).execute(&mut *conn).await.map_err(super::generation::map_query_error)?;
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
        sqlx::query("INSERT INTO read_counts VALUES(?,?,?)")
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
    pub async fn read_row(
        &self,
        snapshot: &Snapshot,
        scope: &str,
        address: &str,
    ) -> Result<ReadRow, GenerationError> {
        let (mut conn, _) = self.read_connection(snapshot).await?;
        let row=sqlx::query("SELECT address,anchor_ref,body_json,scope_epoch,present FROM read_rows WHERE scope_key=? AND address=? AND valid_from<=? ORDER BY valid_from DESC LIMIT 1")
            .bind(scope).bind(address).bind(snapshot.view_revision as i64).fetch_optional(&mut *conn).await.map_err(super::generation::map_query_error)?.ok_or(GenerationError::NotFound)?;
        if row.get::<i64, _>("present") == 0 {
            return Err(GenerationError::NotFound);
        }
        decode(row)
    }

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
        use base64::Engine;
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
                    || &cursor.filter != filter
                {
                    return Err(GenerationError::InvalidCursor);
                }
                Ok(cursor)
            })
            .transpose()?;
        let (mut conn, _) = self.read_connection(snapshot).await?;
        let anchor = if let Some(around) = around {
            sqlx::query("SELECT order_key,address,valid_from FROM read_rows WHERE scope_key=? AND anchor_ref=? AND valid_from<=? AND (valid_to IS NULL OR valid_to>?) AND present=1 ORDER BY order_key,address,valid_from LIMIT 1")
                .bind(scope).bind(around.as_str()).bind(snapshot.view_revision as i64).bind(snapshot.view_revision as i64).fetch_optional(&mut *conn).await.map_err(super::generation::map_query_error)?
        } else {
            None
        };
        if around.is_some() && anchor.is_none() {
            return Err(GenerationError::AnchorOutsideSelection);
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
        let rows = if backward {
            sqlx::query("SELECT order_key,address,anchor_ref,valid_from,valid_to,present,body_bytes,scope_epoch,search_text,directory_state,needs_input FROM read_rows WHERE scope_key=? AND (order_key,address,valid_from)<(?,?,?) ORDER BY order_key DESC,address DESC,valid_from DESC LIMIT ?")
                .bind(scope).bind(&key).bind(&address).bind(version as i64).bind((self.limits().max_candidates+1) as i64).fetch_all(&mut *conn).await.map_err(super::generation::map_query_error)?
        } else {
            sqlx::query("SELECT order_key,address,anchor_ref,valid_from,valid_to,present,body_bytes,scope_epoch,search_text,directory_state,needs_input FROM read_rows WHERE scope_key=? AND (order_key,address,valid_from)>(?,?,?) ORDER BY order_key,address,valid_from LIMIT ?")
                .bind(scope).bind(&key).bind(&address).bind(version as i64).bind((self.limits().max_candidates+1) as i64).fetch_all(&mut *conn).await.map_err(super::generation::map_query_error)?
        };
        let count:Option<i64>=sqlx::query_scalar("SELECT count FROM read_counts WHERE scope_key=? AND valid_from<=? ORDER BY valid_from DESC LIMIT 1").bind(scope).bind(snapshot.view_revision as i64).fetch_optional(&mut *conn).await.map_err(super::generation::map_query_error)?;
        let fetched = rows.len();
        let mut result = Vec::new();
        let mut bytes = 0usize;
        let mut last = None;
        let mut first = None;
        let mut more = fetched > self.limits().max_candidates;
        let mut budget = false;
        for (index, row) in rows
            .into_iter()
            .take(self.limits().max_candidates)
            .enumerate()
        {
            let valid_from = row.get::<i64, _>("valid_from") as u64;
            let valid_to = row.get::<Option<i64>, _>("valid_to");
            let position = Cursor {
                generation: snapshot.generation.clone(),
                revision: snapshot.view_revision,
                scope: scope.into(),
                key: row.get("order_key"),
                address: row.get("address"),
                valid_from,
                backward,
                filter: filter.clone(),
            };
            if row.get::<i64, _>("present") != 0
                && valid_from <= snapshot.view_revision
                && valid_to.is_none_or(|end| end as u64 > snapshot.view_revision)
                && filter.matches(&row)
            {
                let size = row.get::<i64, _>("body_bytes") as usize;
                if bytes + size > self.limits().max_summary_bytes {
                    if result.is_empty() {
                        return Err(GenerationError::Limit("individual reading row"));
                    }
                    more = true;
                    break;
                }
                let body=sqlx::query("SELECT address,anchor_ref,body_json,scope_epoch FROM read_rows WHERE scope_key=? AND address=? AND valid_from=?")
                    .bind(scope).bind(&position.address).bind(valid_from as i64).fetch_one(&mut *conn).await.map_err(super::generation::map_query_error)?;
                result.push(decode(body)?);
                bytes += size;
                if first.is_none() {
                    first = Some(position.clone());
                }
            }
            last = Some(position);
            if result.len() == limit {
                more = index + 1 < fetched;
                break;
            }
        }
        if result.len() < limit && fetched > self.limits().max_candidates {
            budget = true;
        }
        let encode = |mut cursor: Cursor, direction: bool| -> Result<String, GenerationError> {
            cursor.backward = direction;
            Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&cursor)?))
        };
        let has_opposite = cursor.is_some() || around.is_some();
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
            total: count.unwrap_or(0) as u64,
        })
    }
}

fn decode(row: sqlx::sqlite::SqliteRow) -> Result<ReadRow, GenerationError> {
    Ok(ReadRow {
        address: row.try_get("address")?,
        anchor: row
            .try_get::<Option<String>, _>("anchor_ref")?
            .map(TraceAddress::parse)
            .transpose()
            .map_err(|error| GenerationError::Invalid(error.to_string()))?,
        body: serde_json::from_str(&row.try_get::<String, _>("body_json")?)?,
        epoch: IngestionEpoch::new(row.try_get::<String, _>("scope_epoch")?)
            .map_err(|error| GenerationError::Invalid(error.to_string()))?,
    })
}

#[cfg(test)]
mod tests;
