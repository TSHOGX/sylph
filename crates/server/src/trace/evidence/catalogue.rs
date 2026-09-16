//! Publication, reusable view leases and retention roots in evidence's authority.
//!
//! Catalogue transactions never call a mapper, another database, or the filesystem.
//! The publication coordinator supplies verified file identities and fenced cuts.

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};
use sylph_protocol::trace::{
    AgentRef, IngestionEpoch, IngestionScope, Issuer, ObservationRef, OperationalFact, PayloadRef,
    TransportKind,
};

use super::EvidenceStore;
use crate::trace::generation::{GenerationManifest, Snapshot};

pub(super) mod gc_store;
pub(super) mod upload_store;

#[derive(Debug, Clone)]
pub struct CatalogueLimits {
    pub max_build_jobs: usize,
    pub max_leases: usize,
    pub max_subject_leases: usize,
    pub max_lease_scopes: usize,
    pub max_lease_lifetime_ms: i64,
    pub max_pin_batch: usize,
    pub max_pending_uploads: usize,
}

impl Default for CatalogueLimits {
    fn default() -> Self {
        Self {
            max_build_jobs: 16,
            max_leases: 1024,
            max_subject_leases: 64,
            max_lease_scopes: 256,
            max_lease_lifetime_ms: 300_000,
            max_pin_batch: 1024,
            max_pending_uploads: 4096,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogueError {
    #[error("catalogue database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("catalogue encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("invalid catalogue state: {0}")]
    Invalid(String),
    #[error("catalogue limit reached: {0}")]
    Limit(&'static str),
    #[error("the active generation changed before publication")]
    PublicationConflict,
    #[error("another job holds the publication barrier")]
    PublicationBusy,
    #[error("scope deletion invalidated this build or view")]
    DeletedScope,
    #[error("snapshot lease is expired, cancelled or unavailable")]
    SnapshotExpired,
    #[error("the snapshot lease belongs to another subject")]
    SubjectMismatch,
    #[error("generation has not been sealed and registered as ready")]
    NotReady,
    #[error("no active trace generation is published")]
    NoActiveGeneration,
    #[error("payload is being collected; retry publication after collection completes")]
    PayloadDeleting,
    #[error("payload has not been durably staged")]
    PayloadMissing,
}

pub type Result<T> = std::result::Result<T, CatalogueError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivePointer {
    pub generation: Option<String>,
    pub serial: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildTicket {
    pub job_id: String,
    pub manifest: GenerationManifest,
    pub relative_file: String,
    pub expected_pointer: ActivePointer,
    pub base_cut: u64,
    pub visibility_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyManifest {
    pub manifest: GenerationManifest,
    pub relative_file: String,
    pub database_identity: String,
    pub snapshot: Snapshot,
    pub fence_token: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedGeneration {
    pub pointer: ActivePointer,
    pub ready: ReadyManifest,
}

#[derive(Debug, Clone)]
pub struct PublicationGuard {
    pub job_id: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct PendingLease {
    pub lease_id: String,
    pub nonce: String,
    pub generation: String,
    pub subject: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewLease {
    pub token: String,
    pub snapshot: Snapshot,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeProof {
    pub scope: IngestionScope,
    pub epoch: IngestionEpoch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinOwnerKind {
    Generation,
    Search,
    Export,
}

impl PinOwnerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Generation => "generation",
            Self::Search => "search",
            Self::Export => "export",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PayloadPin {
    pub payload: PayloadRef,
    pub epoch: IngestionEpoch,
}

#[derive(Debug, Clone)]
pub struct UploadTicket {
    pub upload_id: String,
    pub payload: PayloadRef,
    pub epoch: IngestionEpoch,
}

#[derive(Clone)]
pub struct EvidenceCatalogue {
    evidence: EvidenceStore,
    limits: CatalogueLimits,
}

impl EvidenceCatalogue {
    pub fn new(evidence: EvidenceStore, limits: CatalogueLimits) -> Result<Self> {
        if [
            limits.max_build_jobs,
            limits.max_leases,
            limits.max_subject_leases,
            limits.max_lease_scopes,
            limits.max_pin_batch,
            limits.max_pending_uploads,
        ]
        .contains(&0)
            || limits.max_lease_lifetime_ms <= 0
        {
            return Err(CatalogueError::Invalid(
                "catalogue bounds must be positive".into(),
            ));
        }
        Ok(Self { evidence, limits })
    }

    pub fn evidence(&self) -> &EvidenceStore {
        &self.evidence
    }

    pub async fn scope_is_readable(&self, proof: &ScopeProof) -> Result<bool> {
        let mut connection = self.evidence.readers.acquire().await?;
        let matches: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ingestion_epochs WHERE epoch=? AND scope_ref=?)",
        )
        .bind(proof.epoch.as_str())
        .bind(proof.scope.encode())
        .fetch_one(&mut *connection)
        .await?;
        if !matches {
            return Ok(false);
        }
        match check_epoch(&mut connection, &proof.epoch).await {
            Ok(()) => Ok(true),
            Err(CatalogueError::DeletedScope) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub async fn scope_proof(&self, epoch: &IngestionEpoch) -> Result<ScopeProof> {
        let json:Option<String>=sqlx::query_scalar("SELECT s.scope_json FROM ingestion_epochs e JOIN ingestion_scopes s ON s.scope_ref=e.scope_ref WHERE e.epoch=?").bind(epoch.as_str()).fetch_optional(&self.evidence.readers).await?;
        Ok(ScopeProof {
            scope: serde_json::from_str(
                &json.ok_or_else(|| CatalogueError::Invalid("unknown ingestion epoch".into()))?,
            )?,
            epoch: epoch.clone(),
        })
    }

    /// Evidence admission and visibility changes have independent clocks. A
    /// tombstone can require a new projection even when no new capture arrived.
    pub async fn active_visibility_is_current(&self) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT j.visibility_revision=c.visibility_revision FROM trace_active a JOIN trace_build_jobs j ON j.generation=a.generation CROSS JOIN trace_catalogue_clock c WHERE a.singleton=1 AND c.singleton=1")
            .fetch_optional(&self.evidence.readers).await?.unwrap_or(false))
    }

    pub async fn pointer(&self) -> Result<ActivePointer> {
        let row =
            sqlx::query("SELECT generation,pointer_serial FROM trace_active WHERE singleton=1")
                .fetch_one(&self.evidence.readers)
                .await?;
        pointer(&row)
    }

    pub async fn begin_build(
        &self,
        manifest: GenerationManifest,
        now_ms: i64,
    ) -> Result<BuildTicket> {
        if manifest.generation.is_empty()
            || serde_json::to_vec(&manifest)?.len() > self.evidence.limits.max_batch_envelope_bytes
        {
            return Err(CatalogueError::Invalid(
                "generation manifest must be bounded and named".into(),
            ));
        }
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let active_jobs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM trace_build_jobs WHERE state IN ('building','ready')",
        )
        .fetch_one(&mut *tx)
        .await?;
        if active_jobs >= self.limits.max_build_jobs as i64 {
            return Err(CatalogueError::Limit("active build jobs"));
        }
        let row =
            sqlx::query("SELECT generation,pointer_serial FROM trace_active WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await?;
        let expected_pointer = pointer(&row)?;
        let base_cut: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(admission_seq),0) FROM observations")
                .fetch_one(&mut *tx)
                .await?;
        let visibility_revision: i64 = sqlx::query_scalar(
            "SELECT visibility_revision FROM trace_catalogue_clock WHERE singleton=1",
        )
        .fetch_one(&mut *tx)
        .await?;
        // Arbitrary opaque generation values never become path components.
        let relative_file = format!(
            "trace-{}.db",
            blake3::hash(manifest.generation.as_bytes()).to_hex()
        );
        let job_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO trace_generations(generation,manifest_json,relative_file,state,created_at_ms) VALUES(?,?,?,'building',?)")
            .bind(&manifest.generation).bind(serde_json::to_string(&manifest)?).bind(&relative_file).bind(now_ms).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO trace_build_jobs(job_id,generation,expected_active,expected_pointer_serial,base_cut,visibility_revision,state,created_at_ms) VALUES(?,?,?,?,?,?,'building',?)")
            .bind(&job_id).bind(&manifest.generation).bind(&expected_pointer.generation).bind(signed(expected_pointer.serial)?).bind(base_cut).bind(visibility_revision).bind(now_ms).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(BuildTicket {
            job_id,
            manifest,
            relative_file,
            expected_pointer,
            base_cut: unsigned(base_cut)?,
            visibility_revision: unsigned(visibility_revision)?,
        })
    }

    pub async fn register_file(&self, ticket: &BuildTicket, database_identity: &str) -> Result<()> {
        if database_identity.is_empty() {
            return Err(CatalogueError::Invalid(
                "database identity is required".into(),
            ));
        }
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        check_ticket(&mut tx, ticket).await?;
        let result = sqlx::query("UPDATE trace_generations SET database_identity=? WHERE generation=? AND state='building' AND (database_identity IS NULL OR database_identity=?)")
            .bind(database_identity).bind(&ticket.manifest.generation).bind(database_identity).execute(&mut *tx).await?;
        if result.rows_affected() != 1 {
            return Err(CatalogueError::Invalid(
                "generation file identity changed".into(),
            ));
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn record_ready(&self, ticket: &BuildTicket, ready: &ReadyManifest) -> Result<()> {
        if ready.manifest != ticket.manifest
            || ready.relative_file != ticket.relative_file
            || ready.snapshot.generation != ticket.manifest.generation
            || ready.snapshot.evidence_cut < ticket.base_cut
        {
            return Err(CatalogueError::Invalid(
                "sealed generation does not match its build ticket".into(),
            ));
        }
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        check_ticket(&mut tx, ticket).await?;
        let row =
            sqlx::query("SELECT database_identity,state FROM trace_generations WHERE generation=?")
                .bind(&ticket.manifest.generation)
                .fetch_one(&mut *tx)
                .await?;
        if row
            .try_get::<Option<String>, _>("database_identity")?
            .as_deref()
            != Some(ready.database_identity.as_str())
        {
            return Err(CatalogueError::Invalid(
                "sealed file identity differs from registration".into(),
            ));
        }
        let current_cut: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(admission_seq),0) FROM observations")
                .fetch_one(&mut *tx)
                .await?;
        if ready.snapshot.evidence_cut > unsigned(current_cut)? {
            return Err(CatalogueError::Invalid(
                "sealed cut is ahead of admitted evidence".into(),
            ));
        }
        let json = serde_json::to_string(ready)?;
        let ready_id = blake3::hash(json.as_bytes()).to_hex().to_string();
        sqlx::query("INSERT INTO trace_ready_manifests VALUES(?,?,?,?,?,?,?) ON CONFLICT(ready_id) DO NOTHING").bind(&ready_id).bind(&ticket.manifest.generation).bind(&ticket.job_id).bind(json).bind(signed(ready.snapshot.evidence_cut)?).bind(signed(ready.snapshot.view_revision)?).bind(&ready.database_identity).execute(&mut *tx).await?;
        sqlx::query("UPDATE trace_generations SET state='ready',ready_id=? WHERE generation=? AND state IN ('building','ready')").bind(&ready_id).bind(&ticket.manifest.generation).execute(&mut *tx).await?;
        sqlx::query("UPDATE trace_build_jobs SET state='ready',ready_id=? WHERE job_id=?")
            .bind(&ready_id)
            .bind(&ticket.job_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// This FULL transaction is the only generation publication linearization point.
    pub async fn acquire_publication(&self, ticket: &BuildTicket) -> Result<PublicationGuard> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        check_ticket(&mut tx, ticket).await?;
        let token = uuid::Uuid::new_v4().to_string();
        let result=sqlx::query("UPDATE trace_publication_guard SET job_id=?,guard_token=? WHERE singleton=1 AND guard_token IS NULL").bind(&ticket.job_id).bind(&token).execute(&mut *tx).await?;
        if result.rows_affected() != 1 {
            return Err(CatalogueError::PublicationBusy);
        }
        tx.commit().await?;
        Ok(PublicationGuard {
            job_id: ticket.job_id.clone(),
            token,
        })
    }

    pub async fn release_publication(&self, guard: &PublicationGuard) -> Result<()> {
        let result=sqlx::query("UPDATE trace_publication_guard SET job_id=NULL,guard_token=NULL WHERE singleton=1 AND job_id=? AND guard_token=?").bind(&guard.job_id).bind(&guard.token).execute(&self.evidence.writer).await?;
        if result.rows_affected() != 1 {
            return Err(CatalogueError::PublicationConflict);
        }
        Ok(())
    }

    /// Only startup recovery calls this after the previous server process is gone.
    /// Ordinary publication retries never steal another live job's durable guard.
    pub async fn recover_publication_guard(&self) -> Result<()> {
        sqlx::query(
            "UPDATE trace_publication_guard SET job_id=NULL,guard_token=NULL WHERE singleton=1",
        )
        .execute(&self.evidence.writer)
        .await?;
        Ok(())
    }

    pub async fn publish(
        &self,
        ticket: &BuildTicket,
        guard: &PublicationGuard,
        old_fenced_cut: u64,
    ) -> Result<PublishedGeneration> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        check_ticket(&mut tx, ticket).await?;
        let held:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_publication_guard WHERE singleton=1 AND job_id=? AND guard_token=?)").bind(&ticket.job_id).bind(&guard.token).fetch_one(&mut *tx).await?;
        if !held || guard.job_id != ticket.job_id {
            return Err(CatalogueError::PublicationConflict);
        }
        let row =
            sqlx::query("SELECT generation,pointer_serial FROM trace_active WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await?;
        let current = pointer(&row)?;
        if current != ticket.expected_pointer {
            return Err(CatalogueError::PublicationConflict);
        }
        let json:Option<String>=sqlx::query_scalar("SELECT r.ready_json FROM trace_ready_manifests r JOIN trace_build_jobs j ON j.ready_id=r.ready_id WHERE j.generation=? AND j.job_id=?").bind(&ticket.manifest.generation).bind(&ticket.job_id).fetch_optional(&mut *tx).await?;
        let ready: ReadyManifest = serde_json::from_str(&json.ok_or(CatalogueError::NotReady)?)?;
        if ready.snapshot.evidence_cut < old_fenced_cut {
            return Err(CatalogueError::Invalid(
                "publication cut would regress".into(),
            ));
        }
        let serial = current
            .serial
            .checked_add(1)
            .ok_or_else(|| CatalogueError::Invalid("publication serial overflow".into()))?;
        sqlx::query(
            "UPDATE trace_generations SET state='retained' WHERE generation=? AND state='active'",
        )
        .bind(&current.generation)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE trace_generations SET state='active',initial_cut=? WHERE generation=? AND state='ready'").bind(signed(ready.snapshot.evidence_cut)?).bind(&ready.manifest.generation).execute(&mut *tx).await?;
        sqlx::query("UPDATE trace_active SET generation=?,pointer_serial=? WHERE singleton=1")
            .bind(&ready.manifest.generation)
            .bind(signed(serial)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE trace_build_jobs SET state='published' WHERE job_id=?")
            .bind(&ticket.job_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(PublishedGeneration {
            pointer: ActivePointer {
                generation: Some(ready.manifest.generation.clone()),
                serial,
            },
            ready,
        })
    }

    pub async fn active_generation(&self) -> Result<Option<PublishedGeneration>> {
        let row=sqlx::query("SELECT a.generation,a.pointer_serial,r.ready_json FROM trace_active a LEFT JOIN trace_generations g ON g.generation=a.generation LEFT JOIN trace_ready_manifests r ON r.ready_id=g.ready_id WHERE a.singleton=1").fetch_one(&self.evidence.readers).await?;
        let pointer = pointer(&row)?;
        let Some(json) = row.try_get::<Option<String>, _>("ready_json")? else {
            return if pointer.generation.is_none() {
                Ok(None)
            } else {
                Err(CatalogueError::NotReady)
            };
        };
        Ok(Some(PublishedGeneration {
            pointer,
            ready: serde_json::from_str(&json)?,
        }))
    }

    pub async fn ready_generation(&self, generation: &str) -> Result<ReadyManifest> {
        let json:Option<String>=sqlx::query_scalar("SELECT r.ready_json FROM trace_ready_manifests r JOIN trace_generations g ON g.ready_id=r.ready_id WHERE g.generation=? AND g.state IN ('ready','active','retained')").bind(generation).fetch_optional(&self.evidence.readers).await?;
        Ok(serde_json::from_str(
            &json.ok_or(CatalogueError::NotReady)?,
        )?)
    }

    pub async fn cancel_build(&self, ticket: &BuildTicket, reason: &str) -> Result<()> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("UPDATE trace_build_jobs SET state='cancelled',error=? WHERE job_id=? AND state IN ('building','ready')").bind(reason).bind(&ticket.job_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE trace_generations SET state='cancelled' WHERE generation=? AND state IN ('building','ready')").bind(&ticket.manifest.generation).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn pending_builds(&self) -> Result<Vec<BuildTicket>> {
        let rows=sqlx::query("SELECT j.*,g.manifest_json,g.relative_file FROM trace_build_jobs j JOIN trace_generations g ON g.generation=j.generation WHERE j.state IN ('building','ready') ORDER BY j.created_at_ms,j.job_id LIMIT ?")
            .bind((self.limits.max_build_jobs+1) as i64).fetch_all(&self.evidence.readers).await?;
        if rows.len() > self.limits.max_build_jobs {
            return Err(CatalogueError::Limit("active build jobs"));
        }
        rows.into_iter()
            .map(|row| {
                Ok(BuildTicket {
                    job_id: row.try_get("job_id")?,
                    manifest: serde_json::from_str(&row.try_get::<String, _>("manifest_json")?)?,
                    relative_file: row.try_get("relative_file")?,
                    expected_pointer: ActivePointer {
                        generation: row.try_get("expected_active")?,
                        serial: unsigned(row.try_get("expected_pointer_serial")?)?,
                    },
                    base_cut: unsigned(row.try_get("base_cut")?)?,
                    visibility_revision: unsigned(row.try_get("visibility_revision")?)?,
                })
            })
            .collect()
    }

    /// Reserve a scope before projection reads it. The fixed epoch prevents a
    /// later lease from retagging an old generation with a re-adoption epoch.
    pub async fn pin_generation_scope(
        &self,
        generation: &str,
        proof: &ScopeProof,
        first_cut: u64,
    ) -> Result<()> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        check_scope(&mut tx, proof).await?;
        let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_generations WHERE generation=? AND state IN ('building','active'))").bind(generation).fetch_one(&mut *tx).await?;
        if !live {
            return Err(CatalogueError::NotReady);
        }
        sqlx::query("INSERT INTO trace_generation_scopes VALUES(?,?,?,?) ON CONFLICT(generation,scope_ref,ingestion_epoch) DO UPDATE SET first_evidence_cut=MIN(first_evidence_cut,excluded.first_evidence_cut)")
            .bind(generation).bind(proof.scope.encode()).bind(proof.epoch.as_str()).bind(signed(first_cut)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Bind inventory retention to a native agent only through a retained server
    /// identity fact. A late old binding is kept even when it discovers deletion.
    pub async fn bind_inventory_agent(
        &self,
        proof: &ScopeProof,
        agent: &AgentRef,
        observation: &ObservationRef,
        now_ms: i64,
    ) -> Result<Option<String>> {
        if !matches!(proof.scope, IngestionScope::Inventory(_))
            || !matches!(agent, AgentRef::Session(_))
            || !proof.scope.permits_subject(agent)
            || !matches!(observation.issuer, Issuer::Server(_))
        {
            return Err(CatalogueError::Invalid(
                "inventory identity proof has incompatible scope".into(),
            ));
        }
        let existing:Option<String>=sqlx::query_scalar("SELECT observation_ref FROM ingestion_scope_bindings WHERE scope_ref=? AND epoch=? AND agent_ref=?").bind(proof.scope.encode()).bind(proof.epoch.as_str()).bind(agent.encode()).fetch_optional(&self.evidence.readers).await?;
        if existing.as_deref() == Some(observation.encode().as_str()) {
            let mut conn = self.evidence.readers.acquire().await?;
            return super::epoch_tombstone(&mut conn, proof.epoch.as_str())
                .await
                .map_err(|e| CatalogueError::Invalid(e.to_string()));
        }
        let row=sqlx::query("SELECT o.envelope_json,c.ingestion_epoch FROM observations o JOIN occurrences c ON c.occurrence_id=o.occurrence_id WHERE o.observation_ref=?")
            .bind(observation.encode()).fetch_optional(&self.evidence.readers).await?.ok_or_else(||CatalogueError::Invalid("identity fact is not admitted".into()))?;
        let capture: sylph_protocol::trace::Capture =
            serde_json::from_str(&row.try_get::<String, _>("envelope_json")?)?;
        if capture.transport_kind != TransportKind::Control
            || row.try_get::<String, _>("ingestion_epoch")? != proof.epoch.as_str()
        {
            return Err(CatalogueError::Invalid(
                "identity fact is outside the inventory epoch".into(),
            ));
        }
        let bytes = self
            .evidence
            .payload_bytes(&capture.payload_ref, self.evidence.limits.max_read_bytes)
            .await
            .map_err(|e| CatalogueError::Invalid(e.to_string()))?;
        let fact: OperationalFact = serde_json::from_slice(&bytes)?;
        let OperationalFact::IdentityBound {
            agent_ref,
            source_evidence,
            ..
        } = fact
        else {
            return Err(CatalogueError::Invalid(
                "capture is not an identity binding fact".into(),
            ));
        };
        if agent_ref != *agent
            || source_evidence.is_empty()
            || source_evidence.len() > self.limits.max_pin_batch
            || source_evidence
                .iter()
                .any(|source| source.issuer != Issuer::Environment(agent.environment().clone()))
        {
            return Err(CatalogueError::Invalid(
                "identity binding needs bounded native evidence from the same environment".into(),
            ));
        }
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let correct_scope: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ingestion_epochs WHERE epoch=? AND scope_ref=?)",
        )
        .bind(proof.epoch.as_str())
        .bind(proof.scope.encode())
        .fetch_one(&mut *tx)
        .await?;
        if !correct_scope {
            return Err(CatalogueError::Invalid(
                "epoch does not belong to this inventory".into(),
            ));
        }
        for source in source_evidence {
            let retained: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM observations WHERE observation_ref=?)",
            )
            .bind(source.encode())
            .fetch_one(&mut *tx)
            .await?;
            if !retained {
                return Err(CatalogueError::Invalid(
                    "native identity evidence has not been admitted".into(),
                ));
            }
        }
        let inserted=sqlx::query("INSERT INTO ingestion_scope_bindings(scope_ref,epoch,agent_ref,observation_ref) VALUES(?,?,?,?) ON CONFLICT DO NOTHING")
            .bind(proof.scope.encode()).bind(proof.epoch.as_str()).bind(agent.encode()).bind(observation.encode()).execute(&mut *tx).await?;
        // A newly opened inventory cannot circumvent an Agent's still-active
        // deletion suppression. Keep this association permanent across re-adoption.
        let suppressed:Option<String>=sqlx::query_scalar("SELECT d.tombstone_id FROM ingestion_scopes s JOIN ingestion_epoch_tombstones d ON d.epoch=s.current_epoch WHERE s.scope_ref=? AND s.suppressed=1")
            .bind(IngestionScope::Agent(agent.clone()).encode()).fetch_optional(&mut *tx).await?;
        if let Some(tombstone) = suppressed {
            sqlx::query("INSERT INTO ingestion_binding_deletions(epoch,tombstone_id) VALUES(?,?) ON CONFLICT(epoch) DO NOTHING").bind(proof.epoch.as_str()).bind(tombstone).execute(&mut *tx).await?;
        }
        let tombstone = super::epoch_tombstone(&mut tx, proof.epoch.as_str())
            .await
            .map_err(|e| CatalogueError::Invalid(e.to_string()))?;
        if inserted.rows_affected() == 1 {
            if let Some(tombstone) = &tombstone {
                record_scope_deletion(
                    &mut tx,
                    &proof.scope.encode(),
                    proof.epoch.as_str(),
                    tombstone,
                    now_ms,
                )
                .await
                .map_err(|e| CatalogueError::Invalid(e.to_string()))?;
            }
        }
        tx.commit().await?;
        Ok(tombstone)
    }

    pub async fn begin_lease(
        &self,
        subject: &str,
        now_ms: i64,
        lifetime_ms: i64,
    ) -> Result<PendingLease> {
        if subject.is_empty() || lifetime_ms <= 0 || lifetime_ms > self.limits.max_lease_lifetime_ms
        {
            return Err(CatalogueError::Invalid(
                "lease subject and lifetime are invalid".into(),
            ));
        }
        let expires = now_ms
            .checked_add(lifetime_ms)
            .ok_or_else(|| CatalogueError::Invalid("lease time overflow".into()))?;
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let total:i64=sqlx::query_scalar("SELECT COUNT(*) FROM trace_view_leases WHERE state IN ('pending','active') AND expires_at_ms>?").bind(now_ms).fetch_one(&mut *tx).await?;
        let own:i64=sqlx::query_scalar("SELECT COUNT(*) FROM trace_view_leases WHERE subject=? AND state IN ('pending','active') AND expires_at_ms>?").bind(subject).bind(now_ms).fetch_one(&mut *tx).await?;
        if total >= self.limits.max_leases as i64 || own >= self.limits.max_subject_leases as i64 {
            return Err(CatalogueError::Limit("active view leases"));
        }
        let generation: Option<String> =
            sqlx::query_scalar("SELECT generation FROM trace_active WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await?;
        let generation = generation.ok_or(CatalogueError::NoActiveGeneration)?;
        let visibility: i64 = sqlx::query_scalar(
            "SELECT visibility_revision FROM trace_catalogue_clock WHERE singleton=1",
        )
        .fetch_one(&mut *tx)
        .await?;
        let lease_id = uuid::Uuid::new_v4().to_string();
        let nonce = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO trace_view_leases(lease_id,subject,generation,pending_nonce,state,visibility_revision,created_at_ms,expires_at_ms) VALUES(?,?,?,?,'pending',?,?,?)")
            .bind(&lease_id).bind(subject).bind(&generation).bind(&nonce).bind(visibility).bind(now_ms).bind(expires).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(PendingLease {
            lease_id,
            nonce,
            generation,
            subject: subject.into(),
            expires_at_ms: expires,
        })
    }

    pub async fn finish_lease(
        &self,
        pending: &PendingLease,
        snapshot: &Snapshot,
        now_ms: i64,
    ) -> Result<ViewLease> {
        if snapshot.generation != pending.generation {
            return Err(CatalogueError::Invalid(
                "lease generation changed during setup".into(),
            ));
        }
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let row=sqlx::query("SELECT l.*,g.initial_cut FROM trace_view_leases l JOIN trace_generations g ON g.generation=l.generation WHERE l.lease_id=?").bind(&pending.lease_id).fetch_optional(&mut *tx).await?.ok_or(CatalogueError::SnapshotExpired)?;
        if row.try_get::<String, _>("subject")? != pending.subject {
            return Err(CatalogueError::SubjectMismatch);
        }
        if row.try_get::<String, _>("pending_nonce")? != pending.nonce
            || row.try_get::<String, _>("state")? != "pending"
            || row.try_get::<i64, _>("expires_at_ms")? <= now_ms
        {
            return Err(CatalogueError::SnapshotExpired);
        }
        let visibility: i64 = sqlx::query_scalar(
            "SELECT visibility_revision FROM trace_catalogue_clock WHERE singleton=1",
        )
        .fetch_one(&mut *tx)
        .await?;
        if row.try_get::<i64, _>("visibility_revision")? != visibility {
            return Err(CatalogueError::DeletedScope);
        }
        let minimum = row
            .try_get::<Option<i64>, _>("initial_cut")?
            .ok_or(CatalogueError::NotReady)?;
        if snapshot.evidence_cut < unsigned(minimum)? {
            return Err(CatalogueError::Invalid(
                "snapshot predates generation publication".into(),
            ));
        }
        sqlx::query("UPDATE trace_view_leases SET state='active',evidence_cut=?,view_revision=? WHERE lease_id=?")
            .bind(signed(snapshot.evidence_cut)?).bind(signed(snapshot.view_revision)?).bind(&pending.lease_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(ViewLease {
            token: pending.lease_id.clone(),
            snapshot: snapshot.clone(),
            expires_at_ms: pending.expires_at_ms,
        })
    }

    pub async fn resolve_lease(
        &self,
        subject: &str,
        token: &str,
        now_ms: i64,
    ) -> Result<ViewLease> {
        let mut conn = self.evidence.readers.acquire().await?;
        resolve_lease(&mut conn, subject, token, now_ms).await
    }

    pub async fn bind_lease_scope(
        &self,
        subject: &str,
        token: &str,
        proof: &ScopeProof,
        now_ms: i64,
    ) -> Result<ViewLease> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let lease = resolve_lease(&mut tx, subject, token, now_ms).await?;
        check_scope(&mut tx, proof).await?;
        let represented:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_generation_scopes WHERE generation=? AND scope_ref=? AND ingestion_epoch=? AND first_evidence_cut<=?)")
            .bind(&lease.snapshot.generation).bind(proof.scope.encode()).bind(proof.epoch.as_str()).bind(signed(lease.snapshot.evidence_cut)?).fetch_one(&mut *tx).await?;
        if !represented {
            return Err(CatalogueError::DeletedScope);
        }
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM trace_lease_scopes WHERE lease_id=?")
                .bind(token)
                .fetch_one(&mut *tx)
                .await?;
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_lease_scopes WHERE lease_id=? AND scope_ref=? AND ingestion_epoch=?)").bind(token).bind(proof.scope.encode()).bind(proof.epoch.as_str()).fetch_one(&mut *tx).await?;
        if !exists && count >= self.limits.max_lease_scopes as i64 {
            return Err(CatalogueError::Limit("scopes per view lease"));
        }
        sqlx::query("INSERT INTO trace_lease_scopes VALUES(?,?,?) ON CONFLICT DO NOTHING")
            .bind(token)
            .bind(proof.scope.encode())
            .bind(proof.epoch.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(lease)
    }

    pub async fn expire_lease(&self, subject: &str, token: &str) -> Result<()> {
        let result=sqlx::query("UPDATE trace_view_leases SET state='cancelled',cancel_reason='released' WHERE lease_id=? AND subject=?").bind(token).bind(subject).execute(&self.evidence.writer).await?;
        if result.rows_affected() != 1 {
            return Err(CatalogueError::SubjectMismatch);
        }
        Ok(())
    }

    /// A damaged published file cannot continue to satisfy previously issued
    /// snapshots even after a replacement generation is ready.
    pub async fn cancel_generation_views(&self, generation: &str) -> Result<()> {
        sqlx::query("UPDATE trace_view_leases SET state='cancelled',cancel_reason='generation unavailable' WHERE generation=? AND state IN ('pending','active')")
            .bind(generation).execute(&self.evidence.writer).await?;
        Ok(())
    }

    /// Install this root while a pending/active view already holds the snapshot.
    /// Cross-database consumers hide their old index before moving its root, and
    /// only mark the replacement ready after all matching documents commit.
    pub async fn pin_revision(
        &self,
        kind: PinOwnerKind,
        owner_id: &str,
        snapshot: &Snapshot,
    ) -> Result<()> {
        if !matches!(kind, PinOwnerKind::Search | PinOwnerKind::Export) || owner_id.is_empty() {
            return Err(CatalogueError::Invalid(
                "revision pin owner is invalid".into(),
            ));
        }
        sqlx::query("INSERT INTO trace_revision_pins VALUES(?,?,?,?,?) ON CONFLICT(owner_kind,owner_id) DO UPDATE SET generation=excluded.generation,evidence_cut=excluded.evidence_cut,view_revision=excluded.view_revision")
            .bind(kind.as_str()).bind(owner_id).bind(&snapshot.generation)
            .bind(signed(snapshot.evidence_cut)?).bind(signed(snapshot.view_revision)?)
            .execute(&self.evidence.writer).await?;
        Ok(())
    }

    /// Call only after invalidating the corresponding index/export job. Existing
    /// result leases remain independent roots for their original checkpoints.
    pub async fn release_revision_pin(&self, kind: PinOwnerKind, owner_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM trace_revision_pins WHERE owner_kind=? AND owner_id=?")
            .bind(kind.as_str())
            .bind(owner_id)
            .execute(&self.evidence.writer)
            .await?;
        Ok(())
    }

    pub async fn retired_searches(&self, now_ms: i64, limit: usize) -> Result<Vec<String>> {
        if limit == 0 || limit > self.limits.max_pin_batch {
            return Err(CatalogueError::Limit("retired search batch"));
        }
        Ok(sqlx::query_scalar("SELECT g.generation FROM trace_generations g WHERE g.state IN ('retained','cancelled','retired') AND NOT EXISTS(SELECT 1 FROM trace_view_leases l WHERE l.generation=g.generation AND l.state IN ('pending','active') AND l.expires_at_ms>?) AND (EXISTS(SELECT 1 FROM trace_revision_pins r WHERE r.owner_kind='search' AND r.owner_id=g.generation) OR EXISTS(SELECT 1 FROM trace_payload_pins p WHERE p.owner_kind='search' AND p.owner_id=g.generation)) ORDER BY g.generation LIMIT ?")
            .bind(now_ms).bind(limit as i64).fetch_all(&self.evidence.readers).await?)
    }

    pub async fn generation_is_pinned(&self, generation: &str, now_ms: i64) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_active WHERE generation=?) OR EXISTS(SELECT 1 FROM trace_build_jobs WHERE generation=? AND state IN ('building','ready')) OR EXISTS(SELECT 1 FROM trace_view_leases WHERE generation=? AND state IN ('pending','active') AND expires_at_ms>?) OR EXISTS(SELECT 1 FROM trace_revision_pins WHERE generation=?)")
            .bind(generation).bind(generation).bind(generation).bind(now_ms).bind(generation).fetch_one(&self.evidence.readers).await?)
    }

    pub async fn minimum_leased_revision(
        &self,
        generation: &str,
        now_ms: i64,
    ) -> Result<Option<u64>> {
        let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_view_leases WHERE generation=? AND state='pending' AND expires_at_ms>?)").bind(generation).bind(now_ms).fetch_one(&self.evidence.readers).await?;
        if pending {
            return Ok(Some(0));
        }
        let value:Option<i64>=sqlx::query_scalar("SELECT MIN(view_revision) FROM (SELECT view_revision FROM trace_view_leases WHERE generation=? AND state='active' AND expires_at_ms>? UNION ALL SELECT view_revision FROM trace_revision_pins WHERE generation=?)").bind(generation).bind(now_ms).bind(generation).fetch_one(&self.evidence.readers).await?;
        value.map(unsigned).transpose()
    }

    pub async fn begin_upload(
        &self,
        kind: PinOwnerKind,
        owner_id: &str,
        payload: PayloadRef,
        epoch: IngestionEpoch,
        now_ms: i64,
    ) -> Result<UploadTicket> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        check_epoch(&mut tx, &epoch).await?;
        reject_deleting(&mut tx, payload.digest()).await?;
        // Only pending uploads consume capacity, so this indexed candidate set is
        // bounded even when completed upload receipts have accumulated for years.
        let abandoned=sqlx::query("SELECT upload_id,scope_epoch FROM trace_pending_uploads WHERE state='pending' ORDER BY upload_id LIMIT ?")
            .bind((self.limits.max_pending_uploads+1) as i64).fetch_all(&mut *tx).await?;
        for pending in abandoned {
            let epoch: String = pending.try_get("scope_epoch")?;
            if super::epoch_tombstone(&mut tx, &epoch)
                .await
                .map_err(|e| CatalogueError::Invalid(e.to_string()))?
                .is_some()
            {
                let id: String = pending.try_get("upload_id")?;
                sqlx::query("UPDATE trace_pending_uploads SET state='cancelled' WHERE upload_id=?")
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
                gc_store::remove_root(&mut tx, "upload", &id, &epoch).await?;
            }
        }
        let pending: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM trace_pending_uploads WHERE state='pending'")
                .fetch_one(&mut *tx)
                .await?;
        if pending >= self.limits.max_pending_uploads as i64 {
            return Err(CatalogueError::Limit("pending payload uploads"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO trace_pending_uploads VALUES(?,?,?,?,?,?,'pending',?)")
            .bind(&id)
            .bind(kind.as_str())
            .bind(owner_id)
            .bind(payload.as_str())
            .bind(payload.digest())
            .bind(epoch.as_str())
            .bind(now_ms)
            .execute(&mut *tx)
            .await?;
        gc_store::add_root(&mut tx, "upload", &id, epoch.as_str(), payload.digest()).await?;
        tx.commit().await?;
        Ok(UploadTicket {
            upload_id: id,
            payload,
            epoch,
        })
    }

    pub async fn finish_upload(&self, ticket: &UploadTicket) -> Result<()> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let row=sqlx::query("SELECT owner_kind,owner_id,payload_ref,scope_epoch,state FROM trace_pending_uploads WHERE upload_id=?").bind(&ticket.upload_id).fetch_one(&mut *tx).await?;
        if row.try_get::<String, _>("payload_ref")? != ticket.payload.as_str()
            || row.try_get::<String, _>("scope_epoch")? != ticket.epoch.as_str()
        {
            return Err(CatalogueError::Invalid("upload identity changed".into()));
        }
        check_epoch(&mut tx, &ticket.epoch).await?;
        reject_deleting(&mut tx, ticket.payload.digest()).await?;
        if row.try_get::<String, _>("state")? == "cancelled" {
            return Err(CatalogueError::DeletedScope);
        }
        require_payload(&mut tx, &ticket.payload).await?;
        sqlx::query("INSERT INTO trace_payload_pins VALUES(?,?,?,?,?) ON CONFLICT DO NOTHING")
            .bind(row.try_get::<String, _>("owner_kind")?)
            .bind(row.try_get::<String, _>("owner_id")?)
            .bind(ticket.payload.as_str())
            .bind(ticket.payload.digest())
            .bind(ticket.epoch.as_str())
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE trace_pending_uploads SET state='pinned' WHERE upload_id=?")
            .bind(&ticket.upload_id)
            .execute(&mut *tx)
            .await?;
        let kind: PinOwnerKind =
            serde_json::from_value(serde_json::Value::String(row.try_get("owner_kind")?))?;
        let owner: String = row.try_get("owner_id")?;
        gc_store::add_payload_roots(
            &mut tx,
            "pin",
            &gc_store::pin_id(kind, &owner, &ticket.payload)?,
            ticket.epoch.as_str(),
            &ticket.payload,
        )
        .await?;
        gc_store::remove_root(&mut tx, "upload", &ticket.upload_id, ticket.epoch.as_str()).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn pin_payloads(
        &self,
        kind: PinOwnerKind,
        owner_id: &str,
        pins: &[PayloadPin],
    ) -> Result<()> {
        if pins.len() > self.limits.max_pin_batch {
            return Err(CatalogueError::Limit("payload pin batch"));
        }
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        for pin in pins {
            check_epoch(&mut tx, &pin.epoch).await?;
            reject_deleting(&mut tx, pin.payload.digest()).await?;
            require_payload(&mut tx, &pin.payload).await?;
            sqlx::query("INSERT INTO trace_payload_pins VALUES(?,?,?,?,?) ON CONFLICT DO NOTHING")
                .bind(kind.as_str())
                .bind(owner_id)
                .bind(pin.payload.as_str())
                .bind(pin.payload.digest())
                .bind(pin.epoch.as_str())
                .execute(&mut *tx)
                .await?;
            gc_store::add_payload_roots(
                &mut tx,
                "pin",
                &gc_store::pin_id(kind, owner_id, &pin.payload)?,
                pin.epoch.as_str(),
                &pin.payload,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn payload_pins(
        &self,
        kind: PinOwnerKind,
        owner_id: &str,
        after: Option<(&PayloadRef, &IngestionEpoch)>,
        limit: usize,
    ) -> Result<Vec<PayloadPin>> {
        if limit == 0 || limit > self.limits.max_pin_batch {
            return Err(CatalogueError::Limit("payload pin page"));
        }
        let (payload, epoch) = after.map_or(("", ""), |(payload, epoch)| {
            (payload.as_str(), epoch.as_str())
        });
        let rows=sqlx::query("SELECT payload_ref,scope_epoch FROM trace_payload_pins WHERE owner_kind=? AND owner_id=? AND (payload_ref,scope_epoch)>(?,?) ORDER BY payload_ref,scope_epoch LIMIT ?")
            .bind(kind.as_str()).bind(owner_id).bind(payload).bind(epoch).bind(limit as i64).fetch_all(&self.evidence.readers).await?;
        rows.into_iter()
            .map(|row| {
                Ok(PayloadPin {
                    payload: PayloadRef::parse(row.try_get::<String, _>("payload_ref")?)
                        .map_err(|e| CatalogueError::Invalid(e.to_string()))?,
                    epoch: IngestionEpoch::new(row.try_get::<String, _>("scope_epoch")?)
                        .map_err(|e| CatalogueError::Invalid(e.to_string()))?,
                })
            })
            .collect()
    }

    pub async fn verify_payload(&self, pin: &PayloadPin) -> Result<()> {
        let mut conn = self.evidence.readers.acquire().await?;
        check_epoch(&mut conn, &pin.epoch).await?;
        drop(conn);
        self.evidence
            .verify_payload(&pin.payload)
            .await
            .map_err(|e| CatalogueError::Invalid(e.to_string()))?;
        if let Some(manifest) = self
            .evidence
            .payload_manifest(&pin.payload)
            .await
            .map_err(|e| CatalogueError::Invalid(e.to_string()))?
        {
            let actual = self
                .evidence
                .blobs
                .get(pin.payload.digest())
                .await
                .map_err(|e| CatalogueError::Invalid(e.to_string()))?
                .ok_or(CatalogueError::PayloadMissing)?;
            if actual
                != manifest
                    .canonical_bytes()
                    .map_err(|e| CatalogueError::Invalid(e.to_string()))?
            {
                return Err(CatalogueError::Invalid(
                    "payload manifest metadata differs from its immutable bytes".into(),
                ));
            }
            let rows=sqlx::query("SELECT chunk_ref,chunk_offset,length,payload_offset FROM payload_chunks WHERE manifest_ref=? ORDER BY ordinal").bind(pin.payload.as_str()).fetch_all(&self.evidence.readers).await?;
            if rows.len() != manifest.chunks.len() {
                return Err(CatalogueError::Invalid(
                    "manifest chunk membership is incomplete".into(),
                ));
            }
            for (row, chunk) in rows.into_iter().zip(manifest.chunks) {
                if row.try_get::<String, _>("chunk_ref")? != chunk.digest.as_str()
                    || unsigned(row.try_get("chunk_offset")?)? != chunk.chunk_offset
                    || unsigned(row.try_get("length")?)? != chunk.length
                    || unsigned(row.try_get("payload_offset")?)? != chunk.payload_offset
                {
                    return Err(CatalogueError::Invalid(
                        "manifest chunk mapping differs from its immutable bytes".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub async fn cancel_upload(&self, ticket: &UploadTicket) -> Result<()> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("UPDATE trace_pending_uploads SET state='cancelled' WHERE upload_id=? AND payload_ref=? AND scope_epoch=? AND state='pending'")
            .bind(&ticket.upload_id).bind(ticket.payload.as_str()).bind(ticket.epoch.as_str()).execute(&mut *tx).await?;
        gc_store::remove_root(&mut tx, "upload", &ticket.upload_id, ticket.epoch.as_str()).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn release_owner_pins(
        &self,
        kind: PinOwnerKind,
        owner_id: &str,
        limit: usize,
    ) -> Result<usize> {
        if limit == 0 || limit > self.limits.max_pin_batch {
            return Err(CatalogueError::Limit("payload unpin batch"));
        }
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let rows=sqlx::query("SELECT payload_ref,scope_epoch FROM trace_payload_pins WHERE owner_kind=? AND owner_id=? ORDER BY payload_ref,scope_epoch LIMIT ?").bind(kind.as_str()).bind(owner_id).bind(limit as i64).fetch_all(&mut *tx).await?;
        for row in &rows {
            let payload = PayloadRef::parse(row.try_get::<String, _>("payload_ref")?)
                .map_err(|e| CatalogueError::Invalid(e.to_string()))?;
            let epoch: String = row.try_get("scope_epoch")?;
            gc_store::remove_root(
                &mut tx,
                "pin",
                &gc_store::pin_id(kind, owner_id, &payload)?,
                &epoch,
            )
            .await?;
            sqlx::query("DELETE FROM trace_payload_pins WHERE owner_kind=? AND owner_id=? AND payload_ref=? AND scope_epoch=?").bind(kind.as_str()).bind(owner_id).bind(payload.as_str()).bind(epoch).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(rows.len())
    }

    /// Tests and the later GC worker use the same final eligibility reservation.
    /// No bytes are removed here; publishing a new reference must retry this fence.
    pub async fn reserve_collection(&self, digest: &str) -> Result<bool> {
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CatalogueError::Invalid("invalid physical digest".into()));
        }
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let pinned = physical_rooted(&mut tx, digest).await?;
        if pinned {
            return Ok(false);
        }
        sqlx::query("INSERT INTO trace_blob_gc VALUES(?,'deleting') ON CONFLICT(physical_digest) DO UPDATE SET state='deleting'").bind(digest).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn release_collection(&self, digest: &str) -> Result<()> {
        sqlx::query("DELETE FROM trace_blob_gc WHERE physical_digest=?")
            .bind(digest)
            .execute(&self.evidence.writer)
            .await?;
        Ok(())
    }
}

pub(super) async fn record_scope_deletion(
    conn: &mut SqliteConnection,
    scope_ref: &str,
    epoch: &str,
    tombstone_id: &str,
    now_ms: i64,
) -> super::Result<()> {
    let revision:i64=sqlx::query_scalar("UPDATE trace_catalogue_clock SET visibility_revision=visibility_revision+1 WHERE singleton=1 RETURNING visibility_revision").fetch_one(&mut *conn).await?;
    sqlx::query("INSERT INTO trace_scope_deletions VALUES(?,?,?,?,?)")
        .bind(revision)
        .bind(scope_ref)
        .bind(epoch)
        .bind(tombstone_id)
        .bind(now_ms)
        .execute(conn)
        .await?;
    Ok(())
}

async fn check_ticket(conn: &mut SqliteConnection, ticket: &BuildTicket) -> Result<()> {
    let row=sqlx::query("SELECT j.*,g.manifest_json,g.relative_file,c.visibility_revision AS current_visibility FROM trace_build_jobs j JOIN trace_generations g ON g.generation=j.generation CROSS JOIN trace_catalogue_clock c WHERE j.job_id=? AND c.singleton=1").bind(&ticket.job_id).fetch_optional(conn).await?.ok_or(CatalogueError::NotReady)?;
    if !matches!(
        row.try_get::<String, _>("state")?.as_str(),
        "building" | "ready"
    ) {
        return Err(CatalogueError::NotReady);
    }
    if row.try_get::<i64, _>("visibility_revision")?
        != row.try_get::<i64, _>("current_visibility")?
    {
        return Err(CatalogueError::DeletedScope);
    }
    if row.try_get::<String, _>("generation")? != ticket.manifest.generation
        || row.try_get::<Option<String>, _>("expected_active")?
            != ticket.expected_pointer.generation
        || unsigned(row.try_get("expected_pointer_serial")?)? != ticket.expected_pointer.serial
        || unsigned(row.try_get("base_cut")?)? != ticket.base_cut
        || unsigned(row.try_get("visibility_revision")?)? != ticket.visibility_revision
        || serde_json::from_str::<GenerationManifest>(&row.try_get::<String, _>("manifest_json")?)?
            != ticket.manifest
        || row.try_get::<String, _>("relative_file")? != ticket.relative_file
    {
        return Err(CatalogueError::Invalid(
            "build ticket differs from its durable record".into(),
        ));
    }
    Ok(())
}

async fn resolve_lease(
    conn: &mut SqliteConnection,
    subject: &str,
    token: &str,
    now_ms: i64,
) -> Result<ViewLease> {
    let row = sqlx::query("SELECT * FROM trace_view_leases WHERE lease_id=?")
        .bind(token)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(CatalogueError::SnapshotExpired)?;
    if row.try_get::<String, _>("subject")? != subject {
        return Err(CatalogueError::SubjectMismatch);
    }
    if row.try_get::<String, _>("state")? != "active"
        || row.try_get::<i64, _>("expires_at_ms")? <= now_ms
    {
        return Err(CatalogueError::SnapshotExpired);
    }
    let deleted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_lease_scopes l JOIN ingestion_epoch_tombstones d ON d.epoch=l.ingestion_epoch WHERE l.lease_id=? AND d.tombstone_id IS NOT NULL)").bind(token).fetch_one(&mut *conn).await?;
    if deleted {
        return Err(CatalogueError::DeletedScope);
    }
    Ok(ViewLease {
        token: token.into(),
        snapshot: Snapshot {
            generation: row.try_get("generation")?,
            evidence_cut: unsigned(row.try_get("evidence_cut")?)?,
            view_revision: unsigned(row.try_get("view_revision")?)?,
        },
        expires_at_ms: row.try_get("expires_at_ms")?,
    })
}

async fn check_scope(conn: &mut SqliteConnection, proof: &ScopeProof) -> Result<()> {
    let matches:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ingestion_epochs e JOIN ingestion_scopes s ON s.scope_ref=e.scope_ref WHERE e.epoch=? AND e.scope_ref=? AND s.current_epoch=e.epoch AND s.suppressed=0 AND NOT EXISTS(SELECT 1 FROM ingestion_epoch_tombstones d WHERE d.epoch=e.epoch AND d.tombstone_id IS NOT NULL))")
        .bind(proof.epoch.as_str()).bind(proof.scope.encode()).fetch_one(conn).await?;
    if !matches {
        return Err(CatalogueError::DeletedScope);
    }
    Ok(())
}

async fn check_epoch(conn: &mut SqliteConnection, epoch: &IngestionEpoch) -> Result<()> {
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ingestion_epochs e JOIN ingestion_scopes s ON s.scope_ref=e.scope_ref WHERE e.epoch=? AND s.current_epoch=e.epoch AND s.suppressed=0 AND NOT EXISTS(SELECT 1 FROM ingestion_epoch_tombstones d WHERE d.epoch=e.epoch AND d.tombstone_id IS NOT NULL))").bind(epoch.as_str()).fetch_one(conn).await?;
    if !valid {
        return Err(CatalogueError::DeletedScope);
    }
    Ok(())
}

async fn require_payload(conn: &mut SqliteConnection, payload: &PayloadRef) -> Result<()> {
    let present: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM payloads WHERE payload_ref=? AND availability='available')",
    )
    .bind(payload.as_str())
    .fetch_one(conn)
    .await?;
    if !present {
        return Err(CatalogueError::PayloadMissing);
    }
    Ok(())
}

async fn reject_deleting(conn: &mut SqliteConnection, digest: &str) -> Result<()> {
    let deleting: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM trace_blob_gc WHERE physical_digest=? AND state='deleting')",
    )
    .bind(digest)
    .fetch_one(conn)
    .await?;
    if deleting {
        return Err(CatalogueError::PayloadDeleting);
    }
    Ok(())
}

async fn physical_rooted(conn: &mut SqliteConnection, digest: &str) -> Result<bool> {
    gc_store::rooted(conn, digest).await
}

fn pointer(row: &sqlx::sqlite::SqliteRow) -> Result<ActivePointer> {
    Ok(ActivePointer {
        generation: row.try_get("generation")?,
        serial: unsigned(row.try_get("pointer_serial")?)?,
    })
}
fn signed(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| CatalogueError::Invalid("integer exceeds SQLite range".into()))
}
fn unsigned(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| CatalogueError::Invalid("negative catalogue quantity".into()))
}

#[cfg(test)]
mod tests;
