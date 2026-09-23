//! Forward preorder scanning shares checked ancestry through a private continuation.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryCursor {
    snapshot: Snapshot,
    root: AgentRef,
    filter: FilterIdentity,
    key: Vec<u8>,
    address: String,
    version: u64,
    matched: u64,
}
#[derive(Debug, Clone)]
pub struct DirectoryPage {
    pub rows: Vec<ReadRow>,
    pub after: Option<String>,
    pub has_more: bool,
    pub total: Option<u64>,
    pub work_exhausted: bool,
    pub examined: usize,
    pub vm_steps: u64,
}
const FIELDS: &str = "scope_key,address,anchor_ref,order_key,valid_from,valid_to,present,body_bytes,scope_epoch,search_text,directory_state,needs_input,required_environments_json,dependency_epochs_json,directory_end,directory_epochs_json";

// Seek the latest version before checking liveness. Filtering out tombstones in
// SQL scans every superseded version of a removed agent. A component move writes
// its old-scope tombstone and new-scope row at the same revision; the live row wins.
async fn anchor(
    conn: &mut sqlx::SqliteConnection,
    snapshot: &Snapshot,
    agent: &AgentRef,
) -> Result<sqlx::sqlite::SqliteRow, GenerationError> {
    let row = sqlx::query(&format!("SELECT {FIELDS},directory_parent FROM read_rows WHERE directory_end IS NOT NULL AND address=? AND valid_from<=? ORDER BY valid_from DESC,present DESC LIMIT 1"))
        .bind(agent.encode()).bind(snapshot.view_revision as i64).fetch_optional(conn).await.map_err(super::super::generation::map_query_error)?.ok_or(GenerationError::NotFound)?;
    if !is_current(&row, snapshot) {
        return Err(GenerationError::NotFound);
    }
    Ok(row)
}

impl GenerationStore {
    pub async fn directory_parent_candidate(
        &self,
        snapshot: &Snapshot,
        child: &AgentRef,
        parent: &AgentRef,
    ) -> Result<ReadCandidate, GenerationError> {
        let (mut conn, _) = self.read_connection(snapshot).await?;
        let row = anchor(&mut conn, snapshot, child).await?;
        if row
            .try_get::<Option<String>, _>("directory_parent")?
            .as_deref()
            != Some(parent.encode().as_str())
        {
            return Err(GenerationError::NotFound);
        }
        let epochs: BTreeSet<IngestionEpoch> =
            serde_json::from_str(&row.try_get::<String, _>("directory_epochs_json")?)?;
        let mut epochs = epochs.into_iter();
        let epoch = epochs.next().ok_or(GenerationError::NotFound)?;
        Ok(ReadCandidate {
            epoch,
            dependency_epochs: epochs.collect(),
            required_environments: BTreeSet::new(),
        })
    }

    /// Discovery walks an indexed interval, never a transitive-closure row set.
    /// The caller binds the continuation to current grants and deletion visibility;
    /// previously scanned incoming edges remain proved only under those same inputs.
    pub async fn directory_page<F, Fut>(
        &self,
        snapshot: &Snapshot,
        root: &AgentRef,
        after: Option<&str>,
        limit: usize,
        filter: &ReadFilter,
        mut authorize: F,
    ) -> Result<DirectoryPage, GenerationError>
    where
        F: FnMut(ReadCandidate) -> Fut,
        Fut: Future<Output = Result<bool, GenerationError>>,
    {
        if limit > self.limits().max_items {
            return Err(GenerationError::Limit("directory page items"));
        }
        let (mut conn, steps) = self.read_connection(snapshot).await?;
        let row = anchor(&mut conn, snapshot, root).await?;
        if !authorize(decode_candidate(&row)?).await? {
            return Err(GenerationError::NotFound);
        }
        let scope: String = row.try_get("scope_key")?;
        let lower: Vec<u8> = row.try_get("order_key")?;
        let upper: Vec<u8> = row.try_get("directory_end")?;
        let identity = filter.identity()?;
        let mut position = if let Some(after) = after {
            if after.len() > self.limits().max_key_bytes * 4 {
                return Err(GenerationError::InvalidCursor);
            }
            let cursor: DirectoryCursor =
                serde_json::from_str(after).map_err(|_| GenerationError::InvalidCursor)?;
            if cursor.snapshot != *snapshot
                || cursor.root != *root
                || cursor.filter != identity
                || cursor.key < lower
                || cursor.key >= upper
            {
                return Err(GenerationError::InvalidCursor);
            }
            cursor
        } else {
            DirectoryCursor {
                snapshot: snapshot.clone(),
                root: root.clone(),
                filter: identity,
                key: lower.clone(),
                address: String::new(),
                version: 0,
                matched: 0,
            }
        };
        // The tighter scalar lower bound matters: SQLite may otherwise choose a
        // root-bound seek and scan the entire prefix before applying the tuple cursor.
        let sql = format!(
            "SELECT {FIELDS} FROM read_rows WHERE directory_end IS NOT NULL AND scope_key=? AND order_key>=? AND order_key<? AND (order_key,address,valid_from)>(?,?,?) ORDER BY order_key,address,valid_from LIMIT ?"
        );
        let budget = self.limits().max_candidates;
        let candidates = sqlx::query(&sql)
            .bind(&scope)
            .bind(&position.key)
            .bind(&upper)
            .bind(&position.key)
            .bind(&position.address)
            .bind(position.version as i64)
            .bind((budget + 1) as i64)
            .fetch_all(&mut *conn)
            .await
            .map_err(super::super::generation::map_query_error)?;
        let fetched = candidates.len();
        let mut scanned = 0;
        let mut result = vec![];
        let mut bytes = 0;
        let mut resume = None;
        for row in candidates.into_iter().take(budget) {
            scanned += 1;
            let key: Vec<u8> = row.try_get("order_key")?;
            if key < position.key {
                continue;
            } // an unavailable incoming edge skipped this subtree
            let address: String = row.try_get("address")?;
            let version = row.try_get::<i64, _>("valid_from")? as u64;
            if !is_current(&row, snapshot) {
                position.key = key;
                position.address = address;
                position.version = version;
                continue;
            }
            if address != root.encode() {
                let epochs: BTreeSet<IngestionEpoch> =
                    serde_json::from_str(&row.try_get::<String, _>("directory_epochs_json")?)?;
                let mut epochs = epochs.into_iter();
                let Some(epoch) = epochs.next() else {
                    return Err(GenerationError::Invalid(
                        "descendant membership lacks a spawn proof".into(),
                    ));
                };
                if !authorize(ReadCandidate {
                    epoch,
                    dependency_epochs: epochs.collect(),
                    required_environments: BTreeSet::new(),
                })
                .await?
                {
                    position.key = row.try_get("directory_end")?;
                    position.address.clear();
                    position.version = 0;
                    if position.key >= upper {
                        break;
                    }
                    continue;
                }
            }
            let candidate = decode_candidate(&row)?;
            let permitted =
                filter.matches(&row) && filter.permits(&candidate) && authorize(candidate).await?;
            if permitted && limit > 0 && resume.is_none() {
                let size = row.try_get::<i64, _>("body_bytes")? as usize;
                if result.len() == limit || bytes + size > self.limits().max_summary_bytes {
                    if result.is_empty() {
                        return Err(GenerationError::Limit("individual directory row"));
                    }
                    // Keep counting authorized metadata within this pass's budget,
                    // but resume before the first matching row we did not return.
                    resume = Some(position.clone());
                } else {
                    let body = sqlx::query("SELECT address,anchor_ref,body_json,scope_epoch,required_environments_json,dependency_epochs_json FROM read_rows WHERE scope_key=? AND address=? AND valid_from=?")
                        .bind(&scope).bind(&address).bind(version as i64).fetch_one(&mut *conn).await.map_err(super::super::generation::map_query_error)?;
                    result.push(decode(body)?);
                    bytes += size;
                }
            }
            position.matched += u64::from(permitted);
            position.key = key;
            position.address = address;
            position.version = version;
        }
        let exhausted = position.key < upper && fetched > scanned;
        let has_more = resume.is_some() || exhausted;
        let total = (!exhausted).then_some(position.matched);
        let continuation = resume.unwrap_or(position);
        let page = DirectoryPage {
            rows: result,
            after: if has_more {
                Some(serde_json::to_string(&continuation)?)
            } else {
                None
            },
            has_more,
            total,
            work_exhausted: exhausted,
            examined: scanned,
            vm_steps: steps.load(std::sync::atomic::Ordering::Relaxed),
        };
        Ok(page)
    }
}
