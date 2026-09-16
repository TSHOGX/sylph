//! Versioned semantic projection and bounded snapshot reads.
//!
//! `docs/unified-trace-storage.md` § Snapshot pages and live changes owns this
//! contract. Membership changes have their own revisions: a streaming summary
//! update cannot multiply the physical rows an ordinary history page examines.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sqlx::{
    pool::PoolConnection,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, Sqlite, SqliteConnection, SqlitePool,
};
use sylph_protocol::trace::{
    AgentRef, AnyObjectRef, Claim, ClaimRef, OwnerRef, PayloadRange, PayloadRef, Recipient,
    Representation, SelectionRef, TraceFactKind, TraceObject,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerationManifest {
    pub generation: String,
    pub semantic_schema: String,
    pub mapper_builds: BTreeMap<String, String>,
    pub reducer_build: String,
    pub capture_inventory_build: String,
    pub encoding_build: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryLimits {
    pub max_items: usize,
    pub max_summary_bytes: usize,
    pub max_preview_bytes: usize,
    pub max_candidates: usize,
    pub max_vm_steps: u64,
    pub max_object_bytes: usize,
    pub max_batch_changes: usize,
    pub max_batch_bytes: usize,
    pub max_key_bytes: usize,
    pub max_payload_ranges: usize,
}

impl Default for QueryLimits {
    fn default() -> Self {
        Self {
            max_items: 64,
            max_summary_bytes: 256 * 1024,
            max_preview_bytes: 2048,
            max_candidates: 512,
            max_vm_steps: 200_000,
            max_object_bytes: 1024 * 1024,
            max_batch_changes: 1024,
            max_batch_bytes: 8 * 1024 * 1024,
            max_key_bytes: 16 * 1024,
            max_payload_ranges: 4096,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    #[error("invalid projection: {0}")]
    Invalid(String),
    #[error("projection exceeds {0}")]
    Limit(&'static str),
    #[error("projection writer is fenced or its writer token is stale")]
    Fenced,
    #[error("projection expected snapshot {expected:?}, current snapshot is {actual:?}")]
    StaleSnapshot {
        expected: Snapshot,
        actual: Snapshot,
    },
    #[error("snapshot is expired or belongs to another generation")]
    SnapshotExpired,
    #[error("snapshot cut and revision do not match a published checkpoint")]
    InvalidSnapshot,
    #[error("invalid or mismatched query cursor")]
    InvalidCursor,
    #[error("object is absent at this snapshot")]
    NotFound,
    #[error("interpretation withdrawn: {0}")]
    InterpretationWithdrawn(String),
    #[error("payload is not reachable through this anchor at this snapshot")]
    PayloadNotReachable,
    #[error("the object does not belong to this selected query scope at the snapshot")]
    AnchorOutsideSelection,
    #[error("query exceeded its SQL VM work budget")]
    WorkBudget,
    #[error("projection claim identity collides with a different value")]
    ClaimCollision,
    #[error("generation file already exists")]
    AlreadyExists,
    #[error("generation manifest does not match the durable file")]
    ManifestMismatch,
    #[error("projection database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("projection filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("projection encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, GenerationError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryMode {
    Selected,
    Authored,
    RetainedArchive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationFilter {
    Spawn,
    Interaction,
    Trigger,
    Nesting,
    Resource,
    HistoryPredecessor,
    ContextLineage,
    ContextMembership,
    Representation,
    Evidence,
    Result,
    Delivery,
    Operation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipFilter {
    All,
    Active,
    NeedsAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Collection {
    History {
        agent: AgentRef,
        selection: Option<SelectionRef>,
    },
    Activations {
        agent: AgentRef,
    },
    AgentRelations {
        agent: AgentRef,
        relation: RelationFilter,
        direction: Direction,
    },
    ObjectRelations {
        object: AnyObjectRef,
        relation: RelationFilter,
    },
}

/// Every option affecting membership or representation participates in cursor
/// hashing. A reusable snapshot is intentionally independent of this scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryScope {
    pub collection: Collection,
    pub history_mode: HistoryMode,
    pub representation: Option<Representation>,
    pub filter: MembershipFilter,
}

impl QueryScope {
    pub fn history(agent: AgentRef, selection: SelectionRef) -> Self {
        Self {
            collection: Collection::History {
                agent,
                selection: Some(selection),
            },
            history_mode: HistoryMode::Selected,
            representation: None,
            filter: MembershipFilter::All,
        }
    }

    pub fn archive(agent: AgentRef) -> Self {
        Self {
            collection: Collection::History {
                agent,
                selection: None,
            },
            history_mode: HistoryMode::RetainedArchive,
            representation: None,
            filter: MembershipFilter::All,
        }
    }

    fn key(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }

    fn columns(&self) -> Result<(&'static str, String, Option<String>)> {
        Ok(match &self.collection {
            Collection::History { agent, selection } => (
                "history",
                agent.encode(),
                selection.as_ref().map(SelectionRef::encode),
            ),
            Collection::Activations { agent } => ("activations", agent.encode(), None),
            Collection::AgentRelations { agent, .. } => ("agent_relations", agent.encode(), None),
            Collection::ObjectRelations { object, .. } => {
                ("object_relations", object_key(object)?, None)
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    pub generation: String,
    pub evidence_cut: u64,
    pub view_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub snapshot: Snapshot,
    pub revision_floor: u64,
    pub fence_token: u64,
    pub writer_mode: WriterMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriterMode {
    Running,
    Fenced,
    Sealed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FenceLease {
    pub token: u64,
    pub checkpoint: Snapshot,
}

#[derive(Debug, Clone)]
pub struct GenerationValidation {
    pub checkpoint: Snapshot,
    pub database_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ObjectUpdate {
    Put {
        object: Box<TraceObject>,
        preview: String,
        preview_truncated: bool,
    },
    Withdraw {
        object_ref: AnyObjectRef,
        reason: String,
    },
}

impl ObjectUpdate {
    pub fn put(object: TraceObject, preview: impl Into<String>) -> Self {
        Self::Put {
            object: Box::new(object),
            preview: preview.into(),
            preview_truncated: false,
        }
    }

    fn reference(&self) -> AnyObjectRef {
        match self {
            Self::Put { object, .. } => object.object_ref(),
            Self::Withdraw { object_ref, .. } => object_ref.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MembershipChange {
    pub scope: QueryScope,
    pub object_ref: AnyObjectRef,
    pub order_key: Vec<u8>,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadAnchor {
    pub via_ref: AnyObjectRef,
    pub containing_agent: AgentRef,
    pub selection: Option<SelectionRef>,
    pub payload_ref: PayloadRef,
    pub body_revision: String,
    pub representation: Representation,
    pub ranges: Vec<PayloadRange>,
}

impl PayloadAnchor {
    fn key(&self) -> Result<String> {
        // Range coverage is a temporal value. It is not part of the relation's
        // identity, allowing a corrected coverage claim to close its old version.
        Ok(blake3::hash(&serde_json::to_vec(&(
            &self.via_ref,
            &self.containing_agent,
            &self.selection,
            &self.payload_ref,
            &self.body_revision,
            self.representation,
        ))?)
        .to_hex()
        .to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadAnchorChange {
    pub anchor: PayloadAnchor,
    pub present: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionBatch {
    pub through_cut: u64,
    /// A bounded interpretation refinement may advance the view at the same evidence cut.
    pub continuation: bool,
    pub claims: Vec<Claim>,
    pub objects: Vec<ObjectUpdate>,
    pub memberships: Vec<MembershipChange>,
    pub payload_anchors: Vec<PayloadAnchorChange>,
    pub mapper_states: BTreeMap<String, serde_json::Value>,
    pub processed_occurrences: Vec<u64>,
    pub payload_derivations: Vec<sylph_protocol::trace::PayloadDerivation>,
    pub read_rows: Vec<super::reading_store::ReadRowChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectSummary {
    pub object_ref: AnyObjectRef,
    pub revision: u64,
    pub preview: String,
    pub preview_truncated: bool,
    pub detail_bytes: u64,
    pub withdrawn_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDetail {
    pub summary: ObjectSummary,
    pub object: TraceObject,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageState {
    Complete,
    More,
    WorkBudgetExhausted,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWork {
    pub anchor_lookups: usize,
    /// Physical metadata rows fetched, including one continuation lookahead.
    pub fetched_candidates: usize,
    /// Physical candidates consumed by this page's continuation.
    pub candidate_rows: usize,
    pub vm_steps: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPage {
    pub snapshot: Snapshot,
    pub objects: Vec<ObjectSummary>,
    pub total_count: u64,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub state: PageState,
    pub returned_count: usize,
    pub returned_bytes: usize,
    pub limits: QueryLimits,
    pub work: ReadWork,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cursor {
    snapshot: Snapshot,
    scope_hash: String,
    order_key: Vec<u8>,
    object_key: String,
    valid_from: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeBatch {
    pub snapshot: Snapshot,
    pub changed: Vec<AnyObjectRef>,
    pub removed: Vec<AnyObjectRef>,
    pub moved: Vec<AnyObjectRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimSummary {
    pub claim_ref: ClaimRef,
    pub fact_kind: TraceFactKind,
    pub mapper_build: String,
    pub rule_id: String,
    pub evidence_count: u64,
    pub detail_bytes: u64,
    pub first_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimPage {
    pub snapshot: Snapshot,
    pub claims: Vec<ClaimSummary>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub returned_bytes: usize,
    pub limits: QueryLimits,
    pub work: ReadWork,
}

pub struct RetainedClaimsPage {
    pub claims: Vec<Claim>,
    pub next: Option<ClaimRef>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimCursor {
    snapshot: Snapshot,
    subject: AnyObjectRef,
    kind: Option<TraceFactKind>,
    first_revision: u64,
    claim_ref: String,
}

#[derive(Clone)]
pub struct GenerationStore {
    path: PathBuf,
    manifest: GenerationManifest,
    writer: SqlitePool,
    readers: SqlitePool,
    limits: QueryLimits,
}

impl GenerationStore {
    pub async fn create(
        path: impl AsRef<Path>,
        manifest: GenerationManifest,
        limits: QueryLimits,
    ) -> Result<Self> {
        validate_manifest(&manifest)?;
        validate_limits(&limits)?;
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    GenerationError::AlreadyExists
                } else {
                    e.into()
                }
            })?;
        file.sync_all().await?;
        drop(file);
        let writer = writer_pool(path).await?;
        let mut tx = writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::raw_sql(include_str!("generation/schema.sql"))
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO generation_manifest VALUES (1, ?, ?)")
            .bind(serde_json::to_string(&manifest)?)
            .bind(uuid::Uuid::new_v4().to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let readers = reader_pool(path).await?;
        Ok(Self {
            path: path.into(),
            manifest,
            writer,
            readers,
            limits,
        })
    }

    pub async fn open(
        path: impl AsRef<Path>,
        manifest: GenerationManifest,
        limits: QueryLimits,
    ) -> Result<Self> {
        validate_manifest(&manifest)?;
        validate_limits(&limits)?;
        let path = path.as_ref();
        let writer = writer_pool(path).await?;
        let stored: String =
            sqlx::query_scalar("SELECT manifest_json FROM generation_manifest WHERE singleton = 1")
                .fetch_one(&writer)
                .await?;
        if serde_json::from_str::<GenerationManifest>(&stored)? != manifest {
            return Err(GenerationError::ManifestMismatch);
        }
        let readers = reader_pool(path).await?;
        Ok(Self {
            path: path.into(),
            manifest,
            writer,
            readers,
            limits,
        })
    }

    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }
    pub fn limits(&self) -> &QueryLimits {
        &self.limits
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn checkpoint(&self) -> Result<Checkpoint> {
        let row = sqlx::query("SELECT * FROM projection_state WHERE singleton = 1")
            .fetch_one(&self.readers)
            .await?;
        decode_checkpoint(&self.manifest.generation, &row)
    }

    pub async fn snapshot_at(&self,revision:u64)->Result<Snapshot> {
        let row=sqlx::query("SELECT r.evidence_cut FROM projection_revisions r JOIN projection_state s ON s.singleton=1 WHERE r.view_revision=? AND r.view_revision>=s.revision_floor")
            .bind(signed(revision)?).fetch_optional(&self.readers).await?.ok_or(GenerationError::SnapshotExpired)?;
        Ok(Snapshot {generation:self.manifest.generation.clone(),evidence_cut:unsigned(row.try_get("evidence_cut")?)?,view_revision:revision})
    }

    /// Publication proofs survive reader-floor advancement and must still match
    /// the exact durable revision, even when later continuations kept the same cut.
    pub(crate) async fn validate_published_snapshot(&self, snapshot: &Snapshot) -> Result<()> {
        if snapshot.generation != self.manifest.generation {
            return Err(GenerationError::InvalidSnapshot);
        }
        let row = sqlx::query("SELECT s.evidence_cut, s.view_revision, r.evidence_cut AS published_cut FROM projection_state s LEFT JOIN projection_revisions r ON r.view_revision = ? WHERE s.singleton = 1")
            .bind(signed(snapshot.view_revision)?).fetch_one(&self.readers).await?;
        if unsigned(row.try_get("evidence_cut")?)? < snapshot.evidence_cut
            || unsigned(row.try_get("view_revision")?)? < snapshot.view_revision
            || row
                .try_get::<Option<i64>, _>("published_cut")?
                .map(unsigned)
                .transpose()?
                != Some(snapshot.evidence_cut)
        {
            return Err(GenerationError::InvalidSnapshot);
        }
        Ok(())
    }

    pub async fn apply(
        &self,
        expected_fence: u64,
        expected_snapshot: &Snapshot,
        batch: &ProjectionBatch,
    ) -> Result<Checkpoint> {
        self.validate_batch(batch)?;
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT * FROM projection_state WHERE singleton = 1")
            .fetch_one(&mut *tx)
            .await?;
        let prior = decode_checkpoint(&self.manifest.generation, &row)?;
        if prior.fence_token != expected_fence || prior.writer_mode != WriterMode::Running {
            return Err(GenerationError::Fenced);
        }
        if &prior.snapshot != expected_snapshot {
            return Err(GenerationError::StaleSnapshot {
                expected: expected_snapshot.clone(),
                actual: prior.snapshot,
            });
        }
        let expected_cut = expected_snapshot.evidence_cut;
        if batch.through_cut < expected_cut || (batch.through_cut == expected_cut &&
            (!batch.continuation || !batch.processed_occurrences.is_empty())) {
            return Err(GenerationError::Invalid(
                "a same-cut interpretation continuation cannot claim new admitted occurrences".into(),
            ));
        }
        let revision = prior
            .snapshot
            .view_revision
            .checked_add(1)
            .ok_or_else(|| GenerationError::Invalid("revision overflow".into()))?;
        sqlx::query("INSERT INTO projection_revisions VALUES (?, ?)")
            .bind(signed(revision)?)
            .bind(signed(batch.through_cut)?)
            .execute(&mut *tx)
            .await?;
        for claim in &batch.claims {
            save_claim(&mut tx, claim, revision).await?;
        }
        let mut changed = BTreeSet::new();
        let mut removed = BTreeSet::new();
        let mut moved = BTreeSet::new();
        for update in &batch.objects {
            let reference = update.reference();
            let key = object_key(&reference)?;
            sqlx::query("INSERT INTO objects (object_ref, object_kind, first_revision) VALUES (?, ?, ?) ON CONFLICT DO NOTHING")
                .bind(&key).bind(object_kind(&reference)).bind(signed(revision)?).execute(&mut *tx).await?;
            sqlx::query(
                "UPDATE object_versions SET valid_to = ? WHERE object_ref = ? AND valid_to IS NULL",
            )
            .bind(signed(revision)?)
            .bind(&key)
            .execute(&mut *tx)
            .await?;
            let (body, owner, preview, truncated, reason) = match update {
                ObjectUpdate::Put {
                    object,
                    preview,
                    preview_truncated,
                } => {
                    changed.insert(reference.clone());
                    (
                        Some(serde_json::to_string(object)?),
                        anchor_owner(object).map(AgentRef::encode),
                        preview.as_str(),
                        *preview_truncated,
                        None,
                    )
                }
                ObjectUpdate::Withdraw { reason, .. } => {
                    removed.insert(reference.clone());
                    (None, None, "", false, Some(reason.as_str()))
                }
            };
            let body_bytes = body.as_ref().map_or(0, String::len);
            sqlx::query("INSERT INTO object_versions (object_ref, valid_from, object_json, containing_agent, object_bytes, preview, preview_truncated, withdrawn_reason) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(&key).bind(signed(revision)?).bind(body).bind(owner).bind(body_bytes as i64).bind(preview).bind(truncated).bind(reason).execute(&mut *tx).await?;
        }
        let mut deltas = BTreeMap::<String, i64>::new();
        for change in &batch.memberships {
            let scope = change.scope.key()?;
            let (kind, owner, selection) = change.scope.columns()?;
            sqlx::query("INSERT INTO query_scopes VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING")
                .bind(&scope)
                .bind(serde_json::to_string(&change.scope)?)
                .bind(kind)
                .bind(owner)
                .bind(selection)
                .execute(&mut *tx)
                .await?;
            let reference = object_key(&change.object_ref)?;
            let previous: Option<Vec<u8>> = sqlx::query_scalar("SELECT order_key FROM memberships WHERE scope_key = ? AND object_ref = ? AND valid_to IS NULL")
                .bind(&scope).bind(&reference).fetch_optional(&mut *tx).await?;
            if change.present && previous.as_ref() == Some(&change.order_key) {
                continue;
            }
            if previous.is_some() {
                sqlx::query("UPDATE memberships SET valid_to = ? WHERE scope_key = ? AND object_ref = ? AND valid_to IS NULL")
                    .bind(signed(revision)?).bind(&scope).bind(&reference).execute(&mut *tx).await?;
                *deltas.entry(scope.clone()).or_default() -= 1;
            }
            if change.present {
                let visible: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM object_versions WHERE object_ref = ? AND valid_to IS NULL AND withdrawn_reason IS NULL)")
                    .bind(&reference).fetch_one(&mut *tx).await?;
                if !visible {
                    return Err(GenerationError::Invalid(
                        "membership refers to an absent or withdrawn object".into(),
                    ));
                }
                sqlx::query("INSERT INTO memberships (scope_key, order_key, object_ref, valid_from) VALUES (?, ?, ?, ?)")
                    .bind(&scope).bind(&change.order_key).bind(&reference).bind(signed(revision)?).execute(&mut *tx).await?;
                *deltas.entry(scope).or_default() += 1;
            }
            moved.insert(change.object_ref.clone());
        }
        for (scope, delta) in deltas {
            let previous: Option<i64> = sqlx::query_scalar("SELECT member_count FROM scope_counts WHERE scope_key = ? ORDER BY valid_from DESC LIMIT 1")
                .bind(&scope).fetch_optional(&mut *tx).await?;
            let count = previous
                .unwrap_or(0)
                .checked_add(delta)
                .filter(|n| *n >= 0)
                .ok_or_else(|| {
                    GenerationError::Invalid("membership count became negative".into())
                })?;
            sqlx::query("INSERT INTO scope_counts VALUES (?, ?, ?)")
                .bind(scope)
                .bind(signed(revision)?)
                .bind(count)
                .execute(&mut *tx)
                .await?;
        }
        for change in &batch.payload_anchors {
            save_anchor(&mut tx, change, revision).await?;
        }
        super::reading_store::apply_rows(&mut tx, &batch.read_rows, revision).await?;
        for derivation in &batch.payload_derivations {
            let json = serde_json::to_string(derivation)?;
            let key = blake3::hash(json.as_bytes()).to_hex().to_string();
            sqlx::query("INSERT INTO payload_derivations(derivation_key,payload_ref,derivation_json,first_revision) VALUES(?,?,?,?) ON CONFLICT DO NOTHING")
                .bind(key).bind(derivation.payload.as_str()).bind(json).bind(signed(revision)?).execute(&mut *tx).await?;
        }
        for occurrence in &batch.processed_occurrences {
            sqlx::query("INSERT INTO processed_occurrences(occurrence_id, at_revision) VALUES (?, ?) ON CONFLICT DO NOTHING")
                .bind(signed(*occurrence)?).bind(signed(revision)?).execute(&mut *tx).await?;
        }
        for (mapper, value) in &batch.mapper_states {
            sqlx::query("INSERT INTO mapper_states VALUES (?, ?, ?) ON CONFLICT(mapper_key) DO UPDATE SET state_json = excluded.state_json, at_revision = excluded.at_revision")
                .bind(mapper).bind(serde_json::to_string(value)?).bind(signed(revision)?).execute(&mut *tx).await?;
        }
        let snapshot = Snapshot {
            generation: self.manifest.generation.clone(),
            evidence_cut: batch.through_cut,
            view_revision: revision,
        };
        let changes = ChangeBatch {
            snapshot,
            changed: changed.into_iter().collect(),
            removed: removed.into_iter().collect(),
            moved: moved.into_iter().collect(),
        };
        let json = serde_json::to_string(&changes)?;
        sqlx::query("INSERT INTO change_batches VALUES (?, ?, ?, ?)")
            .bind(signed(revision)?)
            .bind(signed(batch.through_cut)?)
            .bind(&json)
            .bind(json.len() as i64)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO projection_commit_proofs VALUES(?,?,?,?,?)")
            .bind(signed(revision)?)
            .bind(signed(prior.snapshot.view_revision)?)
            .bind(signed(expected_cut)?)
            .bind(signed(batch.through_cut)?)
            .bind(blake3::hash(&serde_json::to_vec(batch)?).to_hex().as_str())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE projection_state SET evidence_cut = ?, view_revision = ? WHERE singleton = 1",
        )
        .bind(signed(batch.through_cut)?)
        .bind(signed(revision)?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Checkpoint {
            snapshot: changes.snapshot,
            ..prior
        })
    }

    /// BEGIN IMMEDIATE drains in-flight commits. Incrementing the persistent token
    /// rejects old tasks still queued outside SQLite after this fence is released.
    pub async fn fence(&self, expected_token: u64) -> Result<FenceLease> {
        self.fence_inner(expected_token, None).await
    }

    async fn fence_inner(
        &self,
        expected_token: u64,
        entered: Option<&tokio::sync::Notify>,
    ) -> Result<FenceLease> {
        if let Some(entered) = entered {
            entered.notify_one();
        }
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT * FROM projection_state WHERE singleton = 1")
            .fetch_one(&mut *tx)
            .await?;
        let checkpoint = decode_checkpoint(&self.manifest.generation, &row)?;
        if checkpoint.fence_token != expected_token || checkpoint.writer_mode != WriterMode::Running
        {
            return Err(GenerationError::Fenced);
        }
        let token = expected_token
            .checked_add(1)
            .ok_or_else(|| GenerationError::Invalid("writer token overflow".into()))?;
        sqlx::query("UPDATE projection_state SET fence_token = ?, writer_mode = 'fenced' WHERE singleton = 1")
            .bind(signed(token)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(FenceLease {
            token,
            checkpoint: checkpoint.snapshot,
        })
    }

    pub async fn resume(&self, fence: &FenceLease) -> Result<u64> {
        if fence.checkpoint.generation != self.manifest.generation {
            return Err(GenerationError::Fenced);
        }
        let token = fence
            .token
            .checked_add(1)
            .ok_or_else(|| GenerationError::Invalid("writer token overflow".into()))?;
        let result = sqlx::query("UPDATE projection_state SET fence_token = ?, writer_mode = 'running' WHERE singleton = 1 AND fence_token = ? AND writer_mode IN ('fenced','sealed') AND evidence_cut = ? AND view_revision = ?")
            .bind(signed(token)?).bind(signed(fence.token)?).bind(signed(fence.checkpoint.evidence_cut)?).bind(signed(fence.checkpoint.view_revision)?).execute(&self.writer).await?;
        if result.rows_affected() != 1 {
            return Err(GenerationError::Fenced);
        }
        Ok(token)
    }

    /// Seal only a drained immutable publication cut. The catalogue must publish
    /// the resulting identity/cut before resuming incremental projection.
    pub async fn validate_for_publication(&self) -> Result<GenerationValidation> {
        let mut tx = self.readers.begin().await?;
        let row = sqlx::query("SELECT * FROM projection_state WHERE singleton = 1")
            .fetch_one(&mut *tx)
            .await?;
        let checkpoint = decode_checkpoint(&self.manifest.generation, &row)?.snapshot;
        let identity: String = sqlx::query_scalar(
            "SELECT database_identity FROM generation_manifest WHERE singleton = 1",
        )
        .fetch_one(&mut *tx)
        .await?;
        let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
            .fetch_one(&mut *tx)
            .await?;
        if integrity != "ok" {
            return Err(GenerationError::Invalid(
                "generation integrity check failed".into(),
            ));
        }
        if sqlx::query("PRAGMA foreign_key_check")
            .fetch_optional(&mut *tx)
            .await?
            .is_some()
        {
            return Err(GenerationError::Invalid(
                "generation contains an unresolved physical foreign key".into(),
            ));
        }
        tx.commit().await?;
        Ok(GenerationValidation {
            checkpoint,
            database_identity: identity,
        })
    }

    pub async fn seal(
        &self,
        fence: &FenceLease,
        validation: &GenerationValidation,
    ) -> Result<String> {
        if fence.checkpoint.generation != self.manifest.generation {
            return Err(GenerationError::Fenced);
        }
        if validation.checkpoint != fence.checkpoint {
            return Err(GenerationError::Invalid(
                "publication validation does not cover the fenced cut".into(),
            ));
        }
        let identity: String = sqlx::query_scalar(
            "SELECT database_identity FROM generation_manifest WHERE singleton = 1",
        )
        .fetch_one(&self.readers)
        .await?;
        if identity != validation.database_identity {
            return Err(GenerationError::ManifestMismatch);
        }
        let result = sqlx::query("UPDATE projection_state SET writer_mode = 'sealed' WHERE singleton = 1 AND fence_token = ? AND writer_mode = 'fenced' AND evidence_cut = ? AND view_revision = ?")
            .bind(signed(fence.token)?).bind(signed(fence.checkpoint.evidence_cut)?).bind(signed(fence.checkpoint.view_revision)?).execute(&self.writer).await?;
        if result.rows_affected() != 1 {
            return Err(GenerationError::Fenced);
        }
        // FULL WAL commits already persist their bytes. Checkpoint is a bounded
        // attempt, never a requirement to discard a WAL held by another reader.
        sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
            .execute(&self.writer)
            .await?;
        sync_generation_paths(self.path.clone()).await?;
        Ok(identity)
    }

    /// Verify the bounded tail of fully validated atomic units after background
    /// validation. Full database checks never run while the old writer is fenced.
    pub async fn validate_tail(
        &self,
        baseline: &GenerationValidation,
        fence: &FenceLease,
        max_revisions: usize,
    ) -> Result<GenerationValidation> {
        if max_revisions == 0
            || baseline.checkpoint.generation != self.manifest.generation
            || fence.checkpoint.generation != self.manifest.generation
            || baseline.checkpoint.view_revision > fence.checkpoint.view_revision
        {
            return Err(GenerationError::Invalid(
                "tail validation scope is invalid".into(),
            ));
        }
        let mut tx = self.readers.begin().await?;
        let row = sqlx::query("SELECT * FROM projection_state WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
        let current = decode_checkpoint(&self.manifest.generation, &row)?;
        if current.writer_mode != WriterMode::Fenced
            || current.fence_token != fence.token
            || current.snapshot != fence.checkpoint
        {
            return Err(GenerationError::Fenced);
        }
        let identity: String = sqlx::query_scalar(
            "SELECT database_identity FROM generation_manifest WHERE singleton=1",
        )
        .fetch_one(&mut *tx)
        .await?;
        if identity != baseline.database_identity {
            return Err(GenerationError::ManifestMismatch);
        }
        let rows=sqlx::query("SELECT view_revision,previous_revision,previous_cut,evidence_cut,unit_digest FROM projection_commit_proofs WHERE view_revision>? AND view_revision<=? ORDER BY view_revision LIMIT ?")
            .bind(signed(baseline.checkpoint.view_revision)?).bind(signed(fence.checkpoint.view_revision)?).bind((max_revisions+1) as i64).fetch_all(&mut *tx).await?;
        if rows.len() > max_revisions {
            return Err(GenerationError::Limit("publication tail revisions"));
        }
        let mut revision = baseline.checkpoint.view_revision;
        let mut cut = baseline.checkpoint.evidence_cut;
        for row in rows {
            let next = unsigned(row.try_get("view_revision")?)?;
            let next_cut = unsigned(row.try_get("evidence_cut")?)?;
            let digest: String = row.try_get("unit_digest")?;
            if next != revision + 1
                || unsigned(row.try_get("previous_revision")?)? != revision
                || unsigned(row.try_get("previous_cut")?)? != cut
                || next_cut < cut
                || digest.len() != 64
                || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(GenerationError::Invalid(
                    "publication tail has a missing or invalid projection proof".into(),
                ));
            }
            revision = next;
            cut = next_cut;
        }
        if revision != fence.checkpoint.view_revision || cut != fence.checkpoint.evidence_cut {
            return Err(GenerationError::Invalid(
                "publication tail proofs do not reach the fenced checkpoint".into(),
            ));
        }
        tx.commit().await?;
        Ok(GenerationValidation {
            checkpoint: fence.checkpoint.clone(),
            database_identity: identity,
        })
    }

    pub async fn summary(
        &self,
        snapshot: &Snapshot,
        reference: &AnyObjectRef,
    ) -> Result<ObjectSummary> {
        let (mut conn, _) = self.read_connection(snapshot).await?;
        load_summary(&mut conn, reference, snapshot.view_revision).await
    }

    pub async fn object(
        &self,
        snapshot: &Snapshot,
        reference: &AnyObjectRef,
    ) -> Result<ObjectDetail> {
        let (mut conn, _) = self.read_connection(snapshot).await?;
        let summary = load_summary(&mut conn, reference, snapshot.view_revision).await?;
        if let Some(reason) = &summary.withdrawn_reason {
            return Err(GenerationError::InterpretationWithdrawn(reason.clone()));
        }
        if summary.detail_bytes > self.limits.max_object_bytes as u64 {
            return Err(GenerationError::Limit("object detail bytes"));
        }
        let body: String = sqlx::query_scalar(
            "SELECT object_json FROM object_versions WHERE object_ref = ? AND valid_from = ?",
        )
        .bind(object_key(reference)?)
        .bind(signed(summary.revision)?)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_query_error)?;
        Ok(ObjectDetail {
            summary,
            object: serde_json::from_str(&body)?,
            snapshot: snapshot.clone(),
        })
    }

    pub async fn page(
        &self,
        snapshot: &Snapshot,
        scope: &QueryScope,
        cursor: Option<&str>,
        requested_items: usize,
    ) -> Result<QueryPage> {
        if requested_items == 0 || requested_items > self.limits.max_items {
            return Err(GenerationError::Limit("query items"));
        }
        let scope_hash = scope.key()?;
        let cursor = cursor
            .map(|value| decode_cursor(value, self.limits.max_key_bytes * 4))
            .transpose()?;
        if cursor
            .as_ref()
            .is_some_and(|cursor| cursor.snapshot != *snapshot || cursor.scope_hash != scope_hash)
        {
            return Err(GenerationError::InvalidCursor);
        }
        let (mut conn, ops) = self.read_connection(snapshot).await?;
        let count: Option<i64> = sqlx::query_scalar("SELECT member_count FROM scope_counts WHERE scope_key = ? AND valid_from <= ? ORDER BY valid_from DESC LIMIT 1")
            .bind(&scope_hash).bind(signed(snapshot.view_revision)?).fetch_optional(&mut *conn).await.map_err(map_query_error)?;
        // Filtering validity inside this SELECT makes LIMIT count visible rows,
        // allowing an arbitrarily dense superseded prefix to consume unbounded
        // work. Fetch a fixed covering-index window and filter it afterwards.
        let rows = if let Some(cursor) = &cursor {
            sqlx::query("SELECT order_key, object_ref, valid_from, valid_to FROM memberships WHERE scope_key = ? AND (order_key, object_ref, valid_from) > (?, ?, ?) ORDER BY order_key, object_ref, valid_from LIMIT ?")
                .bind(&scope_hash).bind(&cursor.order_key).bind(&cursor.object_key).bind(signed(cursor.valid_from)?).bind((self.limits.max_candidates + 1) as i64).fetch_all(&mut *conn).await.map_err(map_query_error)?
        } else {
            sqlx::query("SELECT order_key, object_ref, valid_from, valid_to FROM memberships WHERE scope_key = ? ORDER BY order_key, object_ref, valid_from LIMIT ?")
                .bind(&scope_hash).bind((self.limits.max_candidates + 1) as i64).fetch_all(&mut *conn).await.map_err(map_query_error)?
        };
        let has_candidates = rows.len() > self.limits.max_candidates;
        let fetched_candidates = rows.len();
        let candidate_count = rows.len().min(self.limits.max_candidates);
        let mut objects = Vec::new();
        let mut returned_bytes = 0usize;
        let mut scanned = 0usize;
        let mut last = cursor;
        let mut state = if has_candidates {
            PageState::WorkBudgetExhausted
        } else {
            PageState::Complete
        };
        for (index, row) in rows
            .into_iter()
            .take(self.limits.max_candidates)
            .enumerate()
        {
            let since = unsigned(row.try_get("valid_from")?)?;
            let until = row
                .try_get::<Option<i64>, _>("valid_to")?
                .map(unsigned)
                .transpose()?;
            let key: String = row.try_get("object_ref")?;
            if since <= snapshot.view_revision
                && until.is_none_or(|until| until > snapshot.view_revision)
            {
                let reference: AnyObjectRef = serde_json::from_str(&key)?;
                let summary = load_summary(&mut conn, &reference, snapshot.view_revision).await?;
                let bytes = serde_json::to_vec(&summary)?.len();
                if returned_bytes + bytes > self.limits.max_summary_bytes {
                    if objects.is_empty() {
                        return Err(GenerationError::Limit("individual query summary bytes"));
                    }
                    state = PageState::More;
                    break;
                }
                objects.push(summary);
                returned_bytes += bytes;
            }
            scanned += 1;
            last = Some(Cursor {
                snapshot: snapshot.clone(),
                scope_hash: scope_hash.clone(),
                order_key: row.try_get("order_key")?,
                object_key: key,
                valid_from: since,
            });
            if objects.len() == requested_items && (index + 1 < candidate_count || has_candidates) {
                state = PageState::More;
                break;
            }
        }
        let has_more = state != PageState::Complete;
        let next_cursor = if has_more {
            Some(encode_cursor(
                last.as_ref().ok_or(GenerationError::InvalidCursor)?,
            )?)
        } else {
            None
        };
        Ok(QueryPage {
            snapshot: snapshot.clone(),
            total_count: unsigned(count.unwrap_or(0))?,
            next_cursor,
            has_more,
            state,
            returned_count: objects.len(),
            returned_bytes,
            limits: self.limits.clone(),
            work: ReadWork {
                anchor_lookups: 0,
                fetched_candidates,
                candidate_rows: scanned,
                vm_steps: ops.load(Ordering::Relaxed),
            },
            objects,
        })
    }

    /// Open a stable deep link without walking the history prefix. The point
    /// lookup seeks the member's latest revision at the snapshot; the ordinary
    /// bounded page then starts at that exact physical member address.
    pub async fn page_at_object(
        &self,
        snapshot: &Snapshot,
        scope: &QueryScope,
        reference: &AnyObjectRef,
        requested_items: usize,
    ) -> Result<QueryPage> {
        let (mut conn, ops) = self.read_connection(snapshot).await?;
        let row = sqlx::query("SELECT order_key, valid_from, valid_to FROM memberships WHERE scope_key = ? AND object_ref = ? AND valid_from <= ? ORDER BY valid_from DESC LIMIT 1")
            .bind(scope.key()?).bind(object_key(reference)?).bind(signed(snapshot.view_revision)?)
            .fetch_optional(&mut *conn).await.map_err(map_query_error)?
            .ok_or(GenerationError::AnchorOutsideSelection)?;
        if row
            .try_get::<Option<i64>, _>("valid_to")?
            .is_some_and(|until| until <= snapshot.view_revision as i64)
        {
            return Err(GenerationError::AnchorOutsideSelection);
        }
        let valid_from = unsigned(row.try_get("valid_from")?)?;
        let cursor = Cursor {
            snapshot: snapshot.clone(),
            scope_hash: scope.key()?,
            order_key: row.try_get("order_key")?,
            object_key: object_key(reference)?,
            valid_from: valid_from.saturating_sub(1),
        };
        drop(conn);
        let mut page = self
            .page(
                snapshot,
                scope,
                Some(&encode_cursor(&cursor)?),
                requested_items,
            )
            .await?;
        page.work.anchor_lookups = 1;
        page.work.vm_steps += ops.load(Ordering::Relaxed);
        Ok(page)
    }

    /// Provenance is indexed from the protocol's one typed subject derivation.
    /// Corrections remain inspectable claims; filtering by kind never parses JSON.
    pub async fn claims_for(
        &self,
        snapshot: &Snapshot,
        subject: &AnyObjectRef,
        kind: Option<TraceFactKind>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ClaimPage> {
        if limit == 0 || limit > self.limits.max_items {
            return Err(GenerationError::Limit("claim page items"));
        }
        let cursor = cursor
            .map(|encoded| {
                if encoded.len() > self.limits.max_key_bytes * 4 {
                    return Err(GenerationError::InvalidCursor);
                }
                let bytes = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .map_err(|_| GenerationError::InvalidCursor)?;
                serde_json::from_slice::<ClaimCursor>(&bytes)
                    .map_err(|_| GenerationError::InvalidCursor)
            })
            .transpose()?;
        if cursor.as_ref().is_some_and(|cursor| {
            cursor.snapshot != *snapshot || cursor.subject != *subject || cursor.kind != kind
        }) {
            return Err(GenerationError::InvalidCursor);
        }
        let (mut conn, ops) = self.read_connection(snapshot).await?;
        let (after_revision, after_claim) = cursor.as_ref().map_or((0, ""), |cursor| {
            (cursor.first_revision, cursor.claim_ref.as_str())
        });
        let rows = if let Some(kind) = kind {
            sqlx::query("SELECT s.first_revision, s.claim_ref, c.fact_kind, c.mapper_build, c.rule_id, c.evidence_count, c.claim_bytes FROM claim_subjects s JOIN semantic_claims c ON c.claim_ref = s.claim_ref WHERE s.subject_ref = ? AND s.fact_kind = ? AND s.first_revision <= ? AND (s.first_revision, s.claim_ref) > (?, ?) ORDER BY s.first_revision, s.claim_ref LIMIT ?")
                .bind(object_key(subject)?).bind(kind.as_str()).bind(signed(snapshot.view_revision)?).bind(signed(after_revision)?).bind(after_claim).bind((limit + 1) as i64).fetch_all(&mut *conn).await.map_err(map_query_error)?
        } else {
            sqlx::query("SELECT s.first_revision, s.claim_ref, c.fact_kind, c.mapper_build, c.rule_id, c.evidence_count, c.claim_bytes FROM claim_subjects s JOIN semantic_claims c ON c.claim_ref = s.claim_ref WHERE s.subject_ref = ? AND s.first_revision <= ? AND (s.first_revision, s.claim_ref) > (?, ?) ORDER BY s.first_revision, s.claim_ref LIMIT ?")
                .bind(object_key(subject)?).bind(signed(snapshot.view_revision)?).bind(signed(after_revision)?).bind(after_claim).bind((limit + 1) as i64).fetch_all(&mut *conn).await.map_err(map_query_error)?
        };
        let fetched = rows.len();
        let mut has_more = fetched > limit;
        let mut claims = Vec::new();
        let mut returned_bytes = 0usize;
        for row in rows.into_iter().take(limit) {
            let kind_value = serde_json::Value::String(row.try_get("fact_kind")?);
            let summary = ClaimSummary {
                claim_ref: ClaimRef::decode(&row.try_get::<String, _>("claim_ref")?)
                    .map_err(|e| GenerationError::Invalid(e.to_string()))?,
                fact_kind: serde_json::from_value(kind_value)?,
                mapper_build: row.try_get("mapper_build")?,
                rule_id: row.try_get("rule_id")?,
                evidence_count: unsigned(row.try_get("evidence_count")?)?,
                detail_bytes: unsigned(row.try_get("claim_bytes")?)?,
                first_revision: unsigned(row.try_get("first_revision")?)?,
            };
            let size = serde_json::to_vec(&summary)?.len();
            if returned_bytes + size > self.limits.max_summary_bytes {
                if claims.is_empty() {
                    return Err(GenerationError::Limit("individual claim summary bytes"));
                }
                has_more = true;
                break;
            }
            returned_bytes += size;
            claims.push(summary);
        }
        let next_cursor = if has_more {
            let last = claims.last().ok_or(GenerationError::InvalidCursor)?;
            Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&ClaimCursor {
                snapshot: snapshot.clone(),
                subject: subject.clone(),
                kind,
                first_revision: last.first_revision,
                claim_ref: last.claim_ref.encode(),
            })?))
        } else {
            None
        };
        Ok(ClaimPage {
            snapshot: snapshot.clone(),
            work: ReadWork {
                fetched_candidates: fetched,
                candidate_rows: claims.len(),
                anchor_lookups: 0,
                vm_steps: ops.load(Ordering::Relaxed),
            },
            claims,
            next_cursor,
            has_more,
            returned_bytes,
            limits: self.limits.clone(),
        })
    }

    pub async fn claim(&self, snapshot: &Snapshot, reference: &ClaimRef) -> Result<Claim> {
        let (mut conn, _) = self.read_connection(snapshot).await?;
        let length: Option<i64> = sqlx::query_scalar(
            "SELECT claim_bytes FROM semantic_claims WHERE claim_ref = ? AND first_revision <= ?",
        )
        .bind(reference.encode())
        .bind(signed(snapshot.view_revision)?)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_query_error)?;
        let length = unsigned(length.ok_or(GenerationError::NotFound)?)?;
        if length > self.limits.max_object_bytes as u64 {
            return Err(GenerationError::Limit("claim detail bytes"));
        }
        let json: String =
            sqlx::query_scalar("SELECT claim_json FROM semantic_claims WHERE claim_ref = ?")
                .bind(reference.encode())
                .fetch_one(&mut *conn)
                .await
                .map_err(map_query_error)?;
        Ok(serde_json::from_str(&json)?)
    }

    /// The API authenticates containing_agent and verifies the lease/tombstone
    /// before this exact body-revision/range reachability check.
    pub async fn authorize_payload(
        &self,
        snapshot: &Snapshot,
        requested: &PayloadAnchor,
        start: u64,
        end_exclusive: u64,
    ) -> Result<()> {
        if start >= end_exclusive {
            return Err(GenerationError::PayloadNotReachable);
        }
        let (mut conn, _) = self.read_connection(snapshot).await?;
        let via = load_summary(&mut conn, &requested.via_ref, snapshot.view_revision).await?;
        if via.withdrawn_reason.is_some() {
            return Err(GenerationError::PayloadNotReachable);
        }
        let row = sqlx::query("SELECT valid_to, ranges_json FROM payload_anchor_versions WHERE anchor_key = ? AND valid_from <= ? ORDER BY valid_from DESC LIMIT 1")
            .bind(requested.key()?).bind(signed(snapshot.view_revision)?).fetch_optional(&mut *conn).await.map_err(map_query_error)?.ok_or(GenerationError::PayloadNotReachable)?;
        if row
            .try_get::<Option<i64>, _>("valid_to")?
            .is_some_and(|until| until <= snapshot.view_revision as i64)
        {
            return Err(GenerationError::PayloadNotReachable);
        }
        let ranges: Vec<PayloadRange> =
            serde_json::from_str(&row.try_get::<String, _>("ranges_json")?)?;
        let mut cursor = start;
        for range in ranges {
            if range.end_exclusive <= cursor {
                continue;
            }
            if range.start > cursor {
                break;
            }
            cursor = range.end_exclusive;
            if cursor >= end_exclusive {
                return Ok(());
            }
        }
        Err(GenerationError::PayloadNotReachable)
    }

    pub async fn mapper_states(
        &self,
        expected_snapshot: &Snapshot,
    ) -> Result<BTreeMap<String, serde_json::Value>> {
        self.mapper_states_inner(expected_snapshot, None).await
    }

    async fn mapper_states_inner(
        &self,
        expected_snapshot: &Snapshot,
        pause: Option<(&tokio::sync::Notify, &tokio::sync::Notify)>,
    ) -> Result<BTreeMap<String, serde_json::Value>> {
        // Mapper rows retain only their current value. Pin the checkpoint and rows
        // in one read transaction; filtering by at_revision would lose overwritten keys.
        let mut tx = self.readers.begin().await?;
        let row = sqlx::query("SELECT * FROM projection_state WHERE singleton = 1")
            .fetch_one(&mut *tx)
            .await?;
        let actual = decode_checkpoint(&self.manifest.generation, &row)?.snapshot;
        if &actual != expected_snapshot {
            return Err(GenerationError::StaleSnapshot {
                expected: expected_snapshot.clone(),
                actual,
            });
        }
        if let Some((entered, resume)) = pause {
            entered.notify_one();
            resume.notified().await;
        }
        let rows = sqlx::query("SELECT mapper_key, state_json FROM mapper_states")
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("mapper_key")?,
                    serde_json::from_str(&row.try_get::<String, _>("state_json")?)?,
                ))
            })
            .collect()
    }

    pub async fn has_processed_occurrence(&self, occurrence: u64) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM processed_occurrences WHERE occurrence_id = ?)",
        )
        .bind(signed(occurrence)?)
        .fetch_one(&self.readers)
        .await?)
    }

    pub async fn payload_derivations(
        &self,
        snapshot: &Snapshot,
        payload: &PayloadRef,
        limit: usize,
    ) -> Result<Vec<sylph_protocol::trace::PayloadDerivation>> {
        if limit == 0 || limit > self.limits.max_items {
            return Err(GenerationError::Limit("payload derivations"));
        }
        let rows:Vec<String>=sqlx::query_scalar("SELECT derivation_json FROM payload_derivations WHERE payload_ref=? AND first_revision<=? ORDER BY first_revision,derivation_key LIMIT ?")
            .bind(payload.as_str()).bind(signed(snapshot.view_revision)?).bind(limit as i64).fetch_all(&self.readers).await?;
        rows.into_iter()
            .map(|json| serde_json::from_str(&json).map_err(GenerationError::from))
            .collect()
    }

    /// Rebuild an in-memory reducer from a fixed generation cut in bounded pages.
    /// Metadata chooses the byte budget before any immutable claim body is loaded.
    pub async fn retained_claims(
        &self,
        snapshot: &Snapshot,
        after: Option<&ClaimRef>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<RetainedClaimsPage> {
        if limit == 0
            || limit > self.limits.max_candidates
            || max_bytes == 0
            || max_bytes > self.limits.max_batch_bytes
        {
            return Err(GenerationError::Limit("retained claim page"));
        }
        let cursor = after.map(ClaimRef::encode).unwrap_or_default();
        let rows=sqlx::query("SELECT claim_ref,claim_bytes FROM semantic_claims WHERE claim_ref > ? AND first_revision <= ? ORDER BY claim_ref LIMIT ?")
            .bind(cursor).bind(signed(snapshot.view_revision)?).bind((limit+1) as i64).fetch_all(&self.readers).await?;
        let mut page = RetainedClaimsPage {
            claims: vec![],
            next: None,
            has_more: rows.len() > limit,
        };
        let mut bytes = 0usize;
        for row in rows.into_iter().take(limit) {
            let size = usize::try_from(unsigned(row.try_get("claim_bytes")?)?)
                .map_err(|_| GenerationError::Limit("claim length"))?;
            if bytes
                .checked_add(size)
                .is_none_or(|total| total > max_bytes)
            {
                if page.claims.is_empty() {
                    return Err(GenerationError::Limit("individual retained claim"));
                }
                page.has_more = true;
                break;
            }
            let reference = ClaimRef::decode(&row.try_get::<String, _>("claim_ref")?)
                .map_err(|error| GenerationError::Invalid(error.to_string()))?;
            page.claims.push(self.claim(snapshot, &reference).await?);
            page.next = Some(reference);
            bytes += size;
        }
        Ok(page)
    }

    /// Called only after the catalogue has expired every lease/search/export pin
    /// below this floor. Logical eviction commits before bounded physical GC.
    pub async fn advance_revision_floor(&self, floor: u64) -> Result<()> {
        let result = sqlx::query("UPDATE projection_state SET revision_floor = ? WHERE singleton = 1 AND revision_floor <= ? AND view_revision >= ?")
            .bind(signed(floor)?).bind(signed(floor)?).bind(signed(floor)?).execute(&self.writer).await?;
        if result.rows_affected() != 1 {
            return Err(GenerationError::Invalid(
                "revision floor cannot regress or exceed the committed revision".into(),
            ));
        }
        Ok(())
    }

    pub async fn changes_after(&self, revision: u64, limit: usize) -> Result<Vec<ChangeBatch>> {
        if limit == 0 || limit > self.limits.max_items {
            return Err(GenerationError::Limit("change batches"));
        }
        let rows = sqlx::query("SELECT batch_bytes, view_revision FROM change_batches WHERE view_revision > ? ORDER BY view_revision LIMIT ?")
            .bind(signed(revision)?).bind(limit as i64).fetch_all(&self.readers).await?;
        let mut bytes = 0usize;
        let mut result = Vec::new();
        for row in rows {
            let size = unsigned(row.try_get("batch_bytes")?)? as usize;
            if bytes + size > self.limits.max_batch_bytes {
                return Err(GenerationError::Limit("change backlog bytes"));
            }
            let json: String =
                sqlx::query_scalar("SELECT batch_json FROM change_batches WHERE view_revision = ?")
                    .bind(row.try_get::<i64, _>("view_revision")?)
                    .fetch_one(&self.readers)
                    .await?;
            result.push(serde_json::from_str(&json)?);
            bytes += size;
        }
        Ok(result)
    }

    pub(crate) async fn read_connection(
        &self,
        snapshot: &Snapshot,
    ) -> Result<(PoolConnection<Sqlite>, Arc<AtomicU64>)> {
        if snapshot.generation != self.manifest.generation {
            return Err(GenerationError::SnapshotExpired);
        }
        let mut conn = self.readers.acquire().await?;
        let ops = Arc::new(AtomicU64::new(0));
        let counter = ops.clone();
        let budget = self.limits.max_vm_steps;
        conn.lock_handle()
            .await?
            .set_progress_handler(1, move || counter.fetch_add(1, Ordering::Relaxed) < budget);
        let row = sqlx::query("SELECT s.revision_floor, r.evidence_cut FROM projection_state s LEFT JOIN projection_revisions r ON r.view_revision = ? WHERE s.singleton = 1")
            .bind(signed(snapshot.view_revision)?).fetch_one(&mut *conn).await.map_err(map_query_error)?;
        if snapshot.view_revision < unsigned(row.try_get("revision_floor")?)? {
            return Err(GenerationError::SnapshotExpired);
        }
        let cut = row
            .try_get::<Option<i64>, _>("evidence_cut")?
            .map(unsigned)
            .transpose()?;
        if cut != Some(snapshot.evidence_cut) {
            return Err(GenerationError::InvalidSnapshot);
        }
        Ok((conn, ops))
    }

    pub(crate) fn validate_batch(&self, batch: &ProjectionBatch) -> Result<()> {
        let count = batch.claims.len()
            + batch.objects.len()
            + batch.memberships.len()
            + batch.payload_anchors.len()
            + batch.mapper_states.len()
            + batch.read_rows.len()
            + batch.processed_occurrences.len()
            + batch.payload_derivations.len();
        if count > self.limits.max_batch_changes {
            return Err(GenerationError::Limit("projection batch changes"));
        }
        if serde_json::to_vec(batch)?.len() > self.limits.max_batch_bytes {
            return Err(GenerationError::Limit("projection batch bytes"));
        }
        for row in &batch.read_rows {
            if row.scope.len()>self.limits.max_key_bytes||row.address.len()>self.limits.max_key_bytes||row.order_key.len()>self.limits.max_key_bytes {return Err(GenerationError::Limit("reading row key"));}
            if row.body.as_ref().map(serde_json::to_vec).transpose()?.is_some_and(|body|body.len()>self.limits.max_summary_bytes) {return Err(GenerationError::Limit("individual reading row bytes"));}
            if serde_json::to_vec(&row.required_environments)?.len()
                + serde_json::to_vec(&row.target_epochs)?.len()
                > self.limits.max_key_bytes
            {
                return Err(GenerationError::Limit("reading authorization metadata bytes"));
            }
            for environment in &row.required_environments {
                sylph_protocol::EnvironmentId::parse(environment)
                    .map_err(|error| GenerationError::Invalid(error.to_string()))?;
            }
        }
        for claim in &batch.claims {
            claim
                .validate()
                .map_err(|e| GenerationError::Invalid(e.to_string()))?;
        }
        let mut objects = BTreeSet::new();
        for update in &batch.objects {
            if !objects.insert(update.reference()) {
                return Err(GenerationError::Invalid(
                    "one batch changes an object twice".into(),
                ));
            }
            if object_key(&update.reference())?.len() > self.limits.max_key_bytes {
                return Err(GenerationError::Limit("object reference bytes"));
            }
            match update {
                ObjectUpdate::Put {
                    object, preview, ..
                } => {
                    if preview.len() > self.limits.max_preview_bytes {
                        return Err(GenerationError::Limit("preview bytes"));
                    }
                    if serde_json::to_vec(object)?.len() > self.limits.max_object_bytes {
                        return Err(GenerationError::Limit("object declaration bytes"));
                    }
                }
                ObjectUpdate::Withdraw { reason, .. }
                    if reason.is_empty() || reason.len() > self.limits.max_preview_bytes =>
                {
                    return Err(GenerationError::Invalid(
                        "withdrawal needs a bounded reason".into(),
                    ))
                }
                _ => {}
            }
        }
        let mut memberships = BTreeSet::new();
        for change in &batch.memberships {
            if !memberships.insert((change.scope.key()?, change.object_ref.clone())) {
                return Err(GenerationError::Invalid(
                    "one batch changes a membership twice".into(),
                ));
            }
            if change.order_key.len() > self.limits.max_key_bytes
                || serde_json::to_vec(&change.scope)?.len() > self.limits.max_key_bytes
            {
                return Err(GenerationError::Limit("query scope or order key bytes"));
            }
        }
        let mut anchors = BTreeSet::new();
        for change in &batch.payload_anchors {
            let anchor = &change.anchor;
            if !matches!(
                anchor.via_ref,
                AnyObjectRef::HistoryEntry(_)
                    | AnyObjectRef::Delivery(_)
                    | AnyObjectRef::Operation(_)
            ) {
                return Err(GenerationError::Invalid(
                    "payload anchor must be a containing entry, delivery or operation".into(),
                ));
            }
            if anchor.body_revision.is_empty()
                || anchor.body_revision.len() > self.limits.max_key_bytes
            {
                return Err(GenerationError::Invalid(
                    "payload body revision must be bounded and nonempty".into(),
                ));
            }
            if anchor.ranges.len() > self.limits.max_payload_ranges {
                return Err(GenerationError::Limit("payload range count"));
            }
            let mut end = 0;
            for range in &anchor.ranges {
                if range.start < end || range.start >= range.end_exclusive {
                    return Err(GenerationError::Invalid(
                        "payload ranges overlap or are reversed".into(),
                    ));
                }
                end = range.end_exclusive;
            }
            if !anchors.insert(anchor.key()?) {
                return Err(GenerationError::Invalid(
                    "one batch changes a payload anchor twice".into(),
                ));
            }
        }
        Ok(())
    }

    pub async fn close(&self) {
        self.readers.close().await;
        self.writer.close().await;
    }
}

async fn writer_pool(path: &Path) -> Result<SqlitePool> {
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .after_release(|connection,_|Box::pin(crate::db::sqlite::reusable_after_release(connection)))
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Full)
                .foreign_keys(true)
                .busy_timeout(std::time::Duration::from_secs(5)),
        )
        .await?)
}

async fn reader_pool(path: &Path) -> Result<SqlitePool> {
    Ok(SqlitePoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("PRAGMA query_only = ON").execute(conn).await?;
                Ok(())
            })
        })
        .after_release(|conn, _| {
            Box::pin(async move {
                conn.lock_handle().await?.remove_progress_handler();
                Ok(true)
            })
        })
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .read_only(true)
                .foreign_keys(true)
                .busy_timeout(std::time::Duration::from_secs(5)),
        )
        .await?)
}

async fn save_claim(conn: &mut SqliteConnection, claim: &Claim, revision: u64) -> Result<()> {
    let key = claim.claim_ref.encode();
    let body = serde_json::to_string(claim)?;
    let existing: Option<String> =
        sqlx::query_scalar("SELECT claim_json FROM semantic_claims WHERE claim_ref = ?")
            .bind(&key)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(existing) = existing {
        return if existing == body {
            Ok(())
        } else {
            Err(GenerationError::ClaimCollision)
        };
    }
    let evidence: BTreeSet<_> = claim.evidence.iter().collect();
    sqlx::query("INSERT INTO semantic_claims (claim_ref, fact_kind, mapper_build, rule_id, evidence_count, claim_bytes, claim_json, first_revision) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&key)
        .bind(claim.fact.kind().as_str())
        .bind(&claim.mapper_build)
        .bind(&claim.rule_id)
        .bind(evidence.len() as i64)
        .bind(body.len() as i64)
        .bind(body)
        .bind(signed(revision)?)
        .execute(&mut *conn)
        .await?;
    for subject in claim
        .fact
        .subject_refs()
        .into_iter()
        .collect::<BTreeSet<_>>()
    {
        sqlx::query("INSERT INTO claim_subjects (subject_ref, first_revision, claim_ref, fact_kind) VALUES (?, ?, ?, ?)")
            .bind(object_key(&subject)?).bind(signed(revision)?).bind(&key).bind(claim.fact.kind().as_str()).execute(&mut *conn).await?;
    }
    for evidence in evidence {
        sqlx::query("INSERT INTO claim_evidence VALUES (?, ?)")
            .bind(&key)
            .bind(evidence.encode())
            .execute(&mut *conn)
            .await?;
    }
    for prior in &claim.supersedes {
        sqlx::query("INSERT INTO claim_supersessions VALUES (?, ?)")
            .bind(&key)
            .bind(prior.encode())
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

async fn save_anchor(
    conn: &mut SqliteConnection,
    change: &PayloadAnchorChange,
    revision: u64,
) -> Result<()> {
    let anchor = &change.anchor;
    let key = anchor.key()?;
    sqlx::query(
        "UPDATE payload_anchor_versions SET valid_to = ? WHERE anchor_key = ? AND valid_to IS NULL",
    )
    .bind(signed(revision)?)
    .bind(&key)
    .execute(&mut *conn)
    .await?;
    if change.present {
        let owner: Option<String> = sqlx::query_scalar("SELECT containing_agent FROM object_versions WHERE object_ref = ? AND valid_to IS NULL AND withdrawn_reason IS NULL")
            .bind(object_key(&anchor.via_ref)?).fetch_optional(&mut *conn).await?.flatten();
        if owner.as_deref() != Some(anchor.containing_agent.encode().as_str()) {
            return Err(GenerationError::Invalid(
                "payload anchor recipient differs from its containing object".into(),
            ));
        }
        sqlx::query("INSERT INTO payload_anchor_versions (anchor_key, via_ref, containing_agent, selection_ref, payload_ref, body_revision, representation, valid_from, ranges_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(key).bind(object_key(&anchor.via_ref)?).bind(anchor.containing_agent.encode()).bind(anchor.selection.as_ref().map(SelectionRef::encode)).bind(anchor.payload_ref.as_str()).bind(&anchor.body_revision).bind(serde_json::to_string(&anchor.representation)?).bind(signed(revision)?).bind(serde_json::to_string(&anchor.ranges)?).execute(conn).await?;
    }
    Ok(())
}

async fn load_summary(
    conn: &mut SqliteConnection,
    reference: &AnyObjectRef,
    revision: u64,
) -> Result<ObjectSummary> {
    let row = sqlx::query("SELECT valid_from, preview, preview_truncated, object_bytes, withdrawn_reason FROM object_versions WHERE object_ref = ? AND valid_from <= ? ORDER BY valid_from DESC LIMIT 1")
        .bind(object_key(reference)?).bind(signed(revision)?).fetch_optional(conn).await.map_err(map_query_error)?.ok_or(GenerationError::NotFound)?;
    Ok(ObjectSummary {
        object_ref: reference.clone(),
        revision: unsigned(row.try_get("valid_from")?)?,
        preview: row.try_get("preview")?,
        preview_truncated: row.try_get("preview_truncated")?,
        detail_bytes: unsigned(row.try_get("object_bytes")?)?,
        withdrawn_reason: row.try_get("withdrawn_reason")?,
    })
}

fn decode_checkpoint(generation: &str, row: &sqlx::sqlite::SqliteRow) -> Result<Checkpoint> {
    let mode = match row.try_get::<String, _>("writer_mode")?.as_str() {
        "running" => WriterMode::Running,
        "fenced" => WriterMode::Fenced,
        "sealed" => WriterMode::Sealed,
        _ => {
            return Err(GenerationError::Invalid(
                "unknown durable writer mode".into(),
            ))
        }
    };
    Ok(Checkpoint {
        snapshot: Snapshot {
            generation: generation.into(),
            evidence_cut: unsigned(row.try_get("evidence_cut")?)?,
            view_revision: unsigned(row.try_get("view_revision")?)?,
        },
        revision_floor: unsigned(row.try_get("revision_floor")?)?,
        fence_token: unsigned(row.try_get("fence_token")?)?,
        writer_mode: mode,
    })
}

fn object_key(reference: &AnyObjectRef) -> Result<String> {
    Ok(serde_json::to_string(reference)?)
}

fn object_kind(reference: &AnyObjectRef) -> &'static str {
    match reference {
        AnyObjectRef::Agent(_) => "agent",
        AnyObjectRef::Activation(_) => "activation",
        AnyObjectRef::Message(_) => "message",
        AnyObjectRef::MessageRevision(_) => "message_revision",
        AnyObjectRef::Delivery(_) => "delivery",
        AnyObjectRef::Operation(_) => "operation",
        AnyObjectRef::Result(_) => "result",
        AnyObjectRef::Resource(_) => "resource",
        AnyObjectRef::HistoryEntry(_) => "history_entry",
        AnyObjectRef::HistorySelection(_) => "history_selection",
        AnyObjectRef::ContextCheckpoint(_) => "context_checkpoint",
        AnyObjectRef::Measurement(_) => "measurement",
    }
}

fn anchor_owner(object: &TraceObject) -> Option<&AgentRef> {
    match object {
        TraceObject::HistoryEntry(entry) => Some(&entry.containing_agent),
        TraceObject::Operation(operation) => match &operation.owner {
            OwnerRef::Agent { agent } => Some(agent),
            OwnerRef::Unresolved { .. } => None,
        },
        TraceObject::Delivery(delivery) => match &delivery.recipient {
            Recipient::Agent {
                agent: OwnerRef::Agent { agent },
            } => Some(agent),
            _ => None,
        },
        _ => None,
    }
}

fn encode_cursor(cursor: &Cursor) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor)?))
}
fn decode_cursor(encoded: &str, max_bytes: usize) -> Result<Cursor> {
    if encoded.len() > max_bytes {
        return Err(GenerationError::InvalidCursor);
    }
    serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| GenerationError::InvalidCursor)?,
    )
    .map_err(|_| GenerationError::InvalidCursor)
}
fn signed(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| GenerationError::Invalid("integer exceeds SQLite range".into()))
}
fn unsigned(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| GenerationError::Invalid("negative stored quantity".into()))
}
pub(crate) fn map_query_error(error: sqlx::Error) -> GenerationError {
    if error
        .as_database_error()
        .is_some_and(|error| error.code().is_some_and(|code| code == "9"))
    {
        GenerationError::WorkBudget
    } else {
        error.into()
    }
}
fn validate_manifest(manifest: &GenerationManifest) -> Result<()> {
    if [
        &manifest.generation,
        &manifest.semantic_schema,
        &manifest.reducer_build,
        &manifest.capture_inventory_build,
        &manifest.encoding_build,
    ]
    .iter()
    .any(|s| s.is_empty() || s.len() > 4096)
        || manifest
            .mapper_builds
            .iter()
            .any(|(k, v)| k.is_empty() || v.is_empty())
    {
        return Err(GenerationError::Invalid(
            "generation builds must be nonempty and bounded".into(),
        ));
    }
    Ok(())
}
fn validate_limits(limits: &QueryLimits) -> Result<()> {
    if [
        limits.max_items,
        limits.max_summary_bytes,
        limits.max_preview_bytes,
        limits.max_candidates,
        limits.max_object_bytes,
        limits.max_batch_changes,
        limits.max_batch_bytes,
        limits.max_key_bytes,
        limits.max_payload_ranges,
    ]
    .contains(&0)
        || limits.max_vm_steps == 0
    {
        return Err(GenerationError::Invalid(
            "query limits must be positive".into(),
        ));
    }
    Ok(())
}
async fn sync_generation_paths(path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        for file in [
            path.clone(),
            PathBuf::from(format!("{}-wal", path.display())),
        ] {
            match std::fs::File::open(file) {
                Ok(file) => file.sync_all()?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .map_err(|e| GenerationError::Invalid(format!("generation sync worker failed: {e}")))??;
    Ok(())
}

#[cfg(test)]
mod tests;
