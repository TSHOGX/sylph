//! Durable operational input and independently retryable evidence export.
//!
//! `docs/unified-trace-storage.md` § Operational facts use an outbox owns this
//! boundary. Committing an export never calls a vendor: only `claim_dispatch`
//! produces a dispatch permit, after recording the attempt in FULL control.db.

use std::{path::Path, time::Duration};

use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
use sylph_protocol::trace::{
    AgentRef, AuthorizationReceipt, Capture, CommandAcceptance, CommandKind, CommandTarget,
    DeliveryFact, IngestionEpoch, Issuer, ObservationRef, OperationalFact, PayloadChunk,
    PayloadCompleteness, PayloadEncoding, PayloadManifest, PayloadRef, RetryPolicy, SourceLocator,
    SourcePosition, SourceRef, TimeBasis, TimeEvidence, TimeUnit, TransportKind,
};
use sylph_protocol::{trace::SubmittedRevision, SessionId};

#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
    #[error("Invalid control request: {0}")]
    Invalid(String),
    #[error("The control request identity already names different input")]
    IdentityConflict,
    #[error("The command cannot be dispatched or changed in its current state")]
    InvalidTransition,
    #[error("Control outbox capacity reached; evidence export must make progress")]
    Capacity,
    #[error("Control fact exceeds the configured byte limit")]
    TooLarge,
    #[error("Evidence receipt does not match the fixed outbox occurrence")]
    ReceiptMismatch,
}

type Result<T> = std::result::Result<T, OutboxError>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    Accepted,
    Dispatching,
    ReceiverAccepted,
    Unknown,
    Rejected,
    Consumed,
    Cancelled,
    Conflicting,
}

impl CommandState {
    fn from_db(value: &str) -> Result<Self> {
        match value {
            "accepted" => Ok(Self::Accepted),
            "dispatching" => Ok(Self::Dispatching),
            "receiver_accepted" => Ok(Self::ReceiverAccepted),
            "unknown" => Ok(Self::Unknown),
            "rejected" => Ok(Self::Rejected),
            "consumed" => Ok(Self::Consumed),
            "cancelled" => Ok(Self::Cancelled),
            "conflicting" => Ok(Self::Conflicting),
            _ => Err(OutboxError::Invalid(
                "unrecognized stored command state".into(),
            )),
        }
    }

    fn as_db(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Dispatching => "dispatching",
            Self::ReceiverAccepted => "receiver_accepted",
            Self::Unknown => "unknown",
            Self::Rejected => "rejected",
            Self::Consumed => "consumed",
            Self::Cancelled => "cancelled",
            Self::Conflicting => "conflicting",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedCommand {
    pub acceptance: CommandAcceptance,
    pub accepted_observation: ObservationRef,
    pub state: CommandState,
    pub dispatch_attempts: u64,
    pub bound_agent: Option<AgentRef>,
    pub identity_observation: Option<ObservationRef>,
    pub current_input: SubmittedRevision,
    pub current_revision: u64,
}

impl AcceptedCommand {
    /// Resolves identity only; authorization and control reachability remain separate.
    pub fn agent(&self) -> Option<&AgentRef> {
        self.acceptance.target.agent().or(self.bound_agent.as_ref())
    }
}

#[derive(Debug, Clone)]
pub struct DispatchPermit {
    pub command: CommandAcceptance,
    pub attempt: u64,
    pub observation_ref: ObservationRef,
}

/// Export stages `payload`, admits this exact capture, and only then marks its receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxOccurrence {
    pub command_id: String,
    pub capture: Capture,
    pub payload: Vec<u8>,
    pub manifest: Option<PayloadManifest>,
}

struct PreparedPayload {
    reference: PayloadRef,
    manifest: Option<PayloadManifest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportDisposition {
    Retained { admission_cut: u64 },
    DiscardedByDeletion,
}

#[derive(Debug, Clone, Copy)]
pub struct OutboxLimits {
    pub max_input_bytes: usize,
    pub max_fact_bytes: usize,
    pub max_pending_facts: u64,
    pub max_pending_bytes: u64,
    pub max_export_items: usize,
    pub max_export_bytes: usize,
    /// Must be no greater than the evidence store's configured chunk maximum.
    pub payload_chunk_bytes: usize,
    pub max_payload_chunks: usize,
    pub max_capture_envelope_bytes: usize,
    pub max_manifest_bytes: usize,
}

impl Default for OutboxLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_fact_bytes: 24 * 1024 * 1024,
            max_pending_facts: 4096,
            max_pending_bytes: 64 * 1024 * 1024,
            max_export_items: 32,
            max_export_bytes: 24 * 1024 * 1024,
            payload_chunk_bytes: 1024 * 1024,
            max_payload_chunks: 4096,
            max_capture_envelope_bytes: 1024 * 1024,
            max_manifest_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxProgress {
    pub pending_count: u64,
    pub pending_bytes: u64,
    pub oldest_pending_age_ms: Option<u64>,
}

#[derive(Clone)]
pub struct ControlStore {
    pub(crate) pool: SqlitePool,
    pub(crate) readers: SqlitePool,
    issuer: Issuer,
    producer_id: String,
    producer_epoch: String,
    limits: OutboxLimits,
}

impl ControlStore {
    pub async fn open(path: &Path, issuer: Issuer, limits: OutboxLimits) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| OutboxError::Invalid(e.to_string()))?;
        }
        issuer
            .validate()
            .map_err(|error| OutboxError::Invalid(error.to_string()))?;
        if !matches!(issuer, Issuer::Server(_)) {
            return Err(OutboxError::Invalid(
                "control producer must be a server issuer".into(),
            ));
        }
        if limits.max_input_bytes == 0
            || limits.max_fact_bytes == 0
            || limits.max_export_items == 0
            || limits.max_export_bytes < limits.max_fact_bytes
            || limits.max_pending_bytes < limits.max_fact_bytes as u64
            || limits.max_pending_facts == 0
            || limits.max_pending_bytes > i64::MAX as u64
            || limits.max_pending_facts > i64::MAX as u64
            || limits.max_export_items > i64::MAX as usize
            || limits.max_fact_bytes > i64::MAX as usize
            || limits.payload_chunk_bytes == 0
            || limits.max_payload_chunks == 0
            || limits.max_capture_envelope_bytes == 0
            || limits.max_manifest_bytes == 0
        {
            return Err(OutboxError::Invalid(
                "inconsistent control outbox limits".into(),
            ));
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        // A short serialized writer avoids deferred-transaction upgrade races. No
        // vendor request or evidence admission holds this connection/transaction.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .after_release(|connection,_|Box::pin(crate::db::sqlite::reusable_after_release(connection)))
            .connect_with(options.clone())
            .await?;
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(&pool)
            .await?;
        sqlx::raw_sql(include_str!("control/schema.sql"))
            .execute(&pool)
            .await?;
        let issuer_json = serde_json::to_string(&issuer)?;
        sqlx::query("INSERT OR IGNORE INTO control_producer VALUES (1, ?, ?, ?, 0, 0, 0)")
            .bind(&issuer_json)
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(uuid::Uuid::new_v4().to_string())
            .execute(&pool)
            .await?;
        let row = sqlx::query("SELECT issuer_json, producer_id, producer_epoch FROM control_producer WHERE singleton = 1")
            .fetch_one(&pool).await?;
        if row.get::<String, _>("issuer_json") != issuer_json {
            return Err(OutboxError::Invalid(
                "control database belongs to another issuer".into(),
            ));
        }
        let readers = crate::db::sqlite::pool(options, 2, true, false).await?;
        Ok(Self {
            pool,
            readers,
            issuer,
            producer_id: row.get("producer_id"),
            producer_epoch: row.get("producer_epoch"),
            limits,
        })
    }

    pub fn limits(&self) -> OutboxLimits {
        self.limits
    }

    pub async fn close(&self) {
        self.readers.close().await;
        self.pool.close().await;
    }

    fn observation(&self, seq: u64) -> ObservationRef {
        ObservationRef {
            issuer: self.issuer.clone(),
            producer_id: self.producer_id.clone(),
            producer_epoch: self.producer_epoch.clone(),
            producer_seq: seq,
        }
    }

    /// A retry returns the original accepted revision and original ingestion epoch.
    pub async fn accept(&self, command: CommandAcceptance) -> Result<AcceptedCommand> {
        self.validate_acceptance(&command)?;
        let target = serde_json::to_string(&command.target)?;
        // Authorization lease/time may change on an HTTP retry. They cannot replace
        // the receipt retained by the first accepted transaction.
        let request_digest = blake3::hash(&serde_json::to_vec(&(
            &command.target,
            command.kind,
            &command.input,
            &command.retry_policy,
            &command.ingestion_authorization_epoch,
            &command.authorization.subject,
        ))?)
        .to_hex()
        .to_string();
        let payload = serde_json::to_vec(&OperationalFact::Accepted {
            command: Box::new(command.clone()),
        })?;
        let command_json = serde_json::to_vec(&command)?;
        if payload.len() > self.limits.max_fact_bytes {
            return Err(OutboxError::TooLarge);
        }
        let prepared = self.prepare_payload(&payload)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let erased: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM deleted_commands WHERE command_id=?)")
                .bind(&command.command_id)
                .fetch_one(&mut *tx)
                .await?;
        if erased {
            return Err(OutboxError::Invalid(
                "command identity was permanently deleted".into(),
            ));
        }
        let existing = sqlx::query("SELECT * FROM trace_commands WHERE command_id = ? OR (origin_subject = ? AND target_ref = ? AND idempotency_key = ?)")
            .bind(&command.command_id).bind(&command.authorization.subject).bind(&target)
            .bind(&command.idempotency_key).fetch_all(&mut *tx).await?;
        if !existing.is_empty() {
            if existing.len() != 1
                || existing[0].get::<String, _>("request_digest") != request_digest
            {
                return Err(OutboxError::IdentityConflict);
            }
            let accepted = self.command_row(&existing[0])?;
            if accepted.acceptance.idempotency_key != command.idempotency_key {
                return Err(OutboxError::IdentityConflict);
            }
            return Ok(accepted);
        }
        let occurrence = self
            .allocate(&mut tx, &command, payload, prepared, command.accepted_at_ms)
            .await?;
        sqlx::query("INSERT INTO trace_commands (command_id,target_ref,command_kind,environment_id,ingestion_epoch,origin_subject,idempotency_key,request_digest,acceptance_json,current_input_json,accepted_seq,state,created_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,'accepted',?)")
            .bind(&command.command_id).bind(target).bind(serde_json::to_string(&command.kind)?).bind(command.target.ingestion_scope().environment().as_str()).bind(command.ingestion_authorization_epoch.as_str()).bind(&command.authorization.subject)
            .bind(&command.idempotency_key).bind(request_digest).bind(command_json).bind(serde_json::to_vec(&command.input)?)
            .bind(occurrence.capture.observation_ref.producer_seq as i64)
            .bind(command.accepted_at_ms).execute(&mut *tx).await?;
        self.insert_occurrence(&mut tx, "accepted", &occurrence, command.accepted_at_ms)
            .await?;
        tx.commit().await?;
        Ok(AcceptedCommand {
            current_input: command.input.clone(),
            current_revision: 0,
            acceptance: command,
            accepted_observation: occurrence.capture.observation_ref,
            state: CommandState::Accepted,
            dispatch_attempts: 0,
            bound_agent: None,
            identity_observation: None,
        })
    }

    fn validate_acceptance(&self, command: &CommandAcceptance) -> Result<()> {
        command
            .target
            .validate()
            .map_err(|error| OutboxError::Invalid(error.to_string()))?;
        if matches!(command.target, CommandTarget::Launch { .. })
            && command.kind != CommandKind::Send
        {
            return Err(OutboxError::Invalid(
                "only an initial send may target a launch inventory".into(),
            ));
        }
        IngestionEpoch::new(command.ingestion_authorization_epoch.as_str())
            .map_err(|error| OutboxError::Invalid(error.to_string()))?;
        if command.command_id.is_empty()
            || command.input.revision.is_empty()
            || command.input.media_type.is_empty()
            || command.authorization.subject.is_empty()
            || command.authorization.decision_ref.is_empty()
            || command
                .idempotency_key
                .as_ref()
                .is_some_and(|key| key.is_empty())
            || matches!(&command.retry_policy, RetryPolicy::VendorIdempotent { key } if key.is_empty())
        {
            return Err(OutboxError::Invalid(
                "required command identity or authorization is empty".into(),
            ));
        }
        if command.input.bytes.len() > self.limits.max_input_bytes {
            return Err(OutboxError::TooLarge);
        }
        Ok(())
    }

    fn command_row(&self, row: &sqlx::sqlite::SqliteRow) -> Result<AcceptedCommand> {
        Ok(AcceptedCommand {
            current_input: serde_json::from_slice(row.get::<&[u8], _>("current_input_json"))?,
            current_revision: row.get::<i64, _>("current_revision") as u64,
            acceptance: serde_json::from_slice(row.get::<&[u8], _>("acceptance_json"))?,
            accepted_observation: self.observation(row.get::<i64, _>("accepted_seq") as u64),
            state: CommandState::from_db(row.get("state"))?,
            dispatch_attempts: row.get::<i64, _>("dispatch_attempts") as u64,
            bound_agent: row
                .get::<Option<&[u8]>, _>("bound_agent_json")
                .map(serde_json::from_slice)
                .transpose()?,
            identity_observation: row
                .get::<Option<i64>, _>("bound_seq")
                .map(|sequence| self.observation(sequence as u64)),
        })
    }

    pub async fn command(&self, command_id: &str) -> Result<Option<AcceptedCommand>> {
        sqlx::query("SELECT * FROM trace_commands WHERE command_id = ?")
            .bind(command_id)
            .fetch_optional(&self.readers)
            .await?
            .as_ref()
            .map(|row| self.command_row(row))
            .transpose()
    }

    pub async fn commands_for(
        &self,
        subject: &str,
        target: &CommandTarget,
        limit: usize,
    ) -> Result<Vec<AcceptedCommand>> {
        let rows=sqlx::query("SELECT * FROM trace_commands WHERE origin_subject=? AND target_ref=? ORDER BY created_at_ms DESC,command_id LIMIT ?").bind(subject).bind(serde_json::to_string(target)?).bind(limit.clamp(1,200) as i64).fetch_all(&self.readers).await?;
        rows.iter().map(|row| self.command_row(row)).collect()
    }

    pub async fn commands_for_agent(
        &self,
        subject: &str,
        agent: &AgentRef,
        limit: usize,
    ) -> Result<Vec<AcceptedCommand>> {
        let target = CommandTarget::Agent {
            agent: agent.clone(),
        };
        let encoded = serde_json::to_vec(agent)?;
        let rows=sqlx::query("SELECT * FROM trace_commands WHERE origin_subject=? AND (target_ref=? OR bound_agent_json=?) ORDER BY created_at_ms DESC,command_id LIMIT ?").bind(subject).bind(serde_json::to_string(&target)?).bind(encoded).bind(limit.clamp(1,200) as i64).fetch_all(&self.readers).await?;
        rows.iter().map(|row| self.command_row(row)).collect()
    }

    pub async fn revise_queued(
        &self,
        id: &str,
        expected_revision: u64,
        input: SubmittedRevision,
        authorization: AuthorizationReceipt,
    ) -> Result<AcceptedCommand> {
        let command = self
            .command(id)
            .await?
            .ok_or(OutboxError::InvalidTransition)?;
        if command.current_revision != expected_revision
            || command.acceptance.kind != CommandKind::Queue
            || command.acceptance.authorization.subject != authorization.subject
            || !matches!(command.acceptance.target, CommandTarget::Agent { .. })
            || input.revision.is_empty()
            || input.revision == command.current_input.revision
        {
            return Err(OutboxError::InvalidTransition);
        }
        if input.bytes.len() > self.limits.max_input_bytes {
            return Err(OutboxError::TooLarge);
        }
        let payload = serde_json::to_vec(&OperationalFact::Revised {
            command_id: id.into(),
            previous_revision: command.current_input.revision.clone(),
            input: input.clone(),
            authorization,
        })?;
        if payload.len() > self.limits.max_fact_bytes {
            return Err(OutboxError::TooLarge);
        }
        let prepared = self.prepare_payload(&payload)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row =
            sqlx::query("SELECT state,current_revision FROM trace_commands WHERE command_id=?")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if row.get::<String, _>("state") != "accepted"
            || row.get::<i64, _>("current_revision") != expected_revision as i64
        {
            return Err(OutboxError::InvalidTransition);
        }
        let next = expected_revision + 1;
        let occurrence = self
            .allocate(
                &mut tx,
                &command.acceptance,
                payload,
                prepared,
                crate::models::now_ms(),
            )
            .await?;
        self.insert_occurrence(
            &mut tx,
            &format!("revision:{next}"),
            &occurrence,
            crate::models::now_ms(),
        )
        .await?;
        sqlx::query(
            "UPDATE trace_commands SET current_input_json=?,current_revision=? WHERE command_id=?",
        )
        .bind(serde_json::to_vec(&input)?)
        .bind(next as i64)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.command(id)
            .await?
            .ok_or(OutboxError::InvalidTransition)
    }

    pub async fn cancel_queued(
        &self,
        id: &str,
        expected_revision: u64,
        subject: &str,
    ) -> Result<AcceptedCommand> {
        let command = self
            .command(id)
            .await?
            .ok_or(OutboxError::InvalidTransition)?;
        if command.current_revision != expected_revision
            || command.acceptance.kind != CommandKind::Queue
            || command.acceptance.authorization.subject != subject
        {
            return Err(OutboxError::InvalidTransition);
        }
        let fact = DeliveryFact::Cancelled {
            reason: "queued submission cancelled before dispatch".into(),
        };
        let payload = serde_json::to_vec(&OperationalFact::Delivery {
            command_id: id.into(),
            input_revision: command.current_input.revision.clone(),
            fact,
        })?;
        let prepared = self.prepare_payload(&payload)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row =
            sqlx::query("SELECT state,current_revision FROM trace_commands WHERE command_id=?")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if row.get::<String, _>("state") != "accepted"
            || row.get::<i64, _>("current_revision") != expected_revision as i64
        {
            return Err(OutboxError::InvalidTransition);
        }
        let occurrence = self
            .allocate(
                &mut tx,
                &command.acceptance,
                payload,
                prepared,
                crate::models::now_ms(),
            )
            .await?;
        self.insert_occurrence(
            &mut tx,
            "cancel-before-dispatch",
            &occurrence,
            crate::models::now_ms(),
        )
        .await?;
        sqlx::query("UPDATE trace_commands SET state='cancelled' WHERE command_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.command(id)
            .await?
            .ok_or(OutboxError::InvalidTransition)
    }

    pub async fn queued_commands(
        &self,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<AcceptedCommand>> {
        let rows=sqlx::query("SELECT c.* FROM trace_commands c WHERE c.state='accepted' AND c.command_kind=? AND c.accepted_seq>? AND NOT EXISTS(SELECT 1 FROM trace_commands earlier WHERE earlier.target_ref=c.target_ref AND earlier.state='accepted' AND earlier.command_kind=c.command_kind AND earlier.accepted_seq<c.accepted_seq) ORDER BY c.accepted_seq LIMIT ?").bind(serde_json::to_string(&CommandKind::Queue)?).bind(after_sequence.min(i64::MAX as u64) as i64).bind(limit.clamp(1,64) as i64).fetch_all(&self.readers).await?;
        rows.iter().map(|row| self.command_row(row)).collect()
    }

    pub async fn erase_agent_input(&self, agent: &AgentRef) -> Result<usize> {
        sqlx::query("INSERT INTO control_erase_jobs SELECT ?,next_seq,? FROM control_producer WHERE singleton=1 ON CONFLICT(agent_ref) DO UPDATE SET through_seq=MAX(through_seq,excluded.through_seq)").bind(agent.encode()).bind(crate::models::now_ms()).execute(&self.pool).await?;
        self.erase_agent_batch(agent).await
    }

    async fn erase_agent_batch(&self, agent: &AgentRef) -> Result<usize> {
        let target = serde_json::to_string(&CommandTarget::Agent {
            agent: agent.clone(),
        })?;
        let agent = serde_json::to_vec(agent)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let encoded: AgentRef = serde_json::from_slice(&agent)?;
        let cut: i64 =
            sqlx::query_scalar("SELECT through_seq FROM control_erase_jobs WHERE agent_ref=?")
                .bind(encoded.encode())
                .fetch_one(&mut *tx)
                .await?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT c.command_id FROM trace_commands c WHERE (c.target_ref=? OR c.bound_agent_json=?) AND c.accepted_seq<=? AND NOT EXISTS(SELECT 1 FROM control_outbox o WHERE o.command_id=c.command_id AND o.exported_disposition IS NULL) LIMIT 256")
            .bind(target).bind(agent).bind(cut).fetch_all(&mut *tx).await?;
        for id in &ids {
            sqlx::query("INSERT INTO deleted_commands SELECT command_id,origin_subject,target_ref,idempotency_key,ingestion_epoch,? FROM trace_commands WHERE command_id=? ON CONFLICT(command_id) DO NOTHING").bind(crate::models::now_ms()).bind(id).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM control_outbox WHERE command_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM trace_commands WHERE command_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM env_command_results WHERE request_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(ids.len())
    }

    pub async fn erase_pending_inputs(&self) -> Result<usize> {
        let jobs: Vec<String> = sqlx::query_scalar(
            "SELECT agent_ref FROM control_erase_jobs ORDER BY created_at_ms LIMIT 8",
        )
        .fetch_all(&self.readers)
        .await?;
        let mut count = 0;
        for key in jobs {
            let agent = AgentRef::decode(&key).map_err(|e| OutboxError::Invalid(e.to_string()))?;
            let erased = self.erase_agent_batch(&agent).await?;
            count += erased;
            if erased == 0 {
                let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_commands WHERE (target_ref=? OR bound_agent_json=?) AND accepted_seq<=(SELECT through_seq FROM control_erase_jobs WHERE agent_ref=?))").bind(serde_json::to_string(&CommandTarget::Agent{agent:agent.clone()})?).bind(serde_json::to_vec(&agent)?).bind(&key).fetch_one(&self.readers).await?;
                if !pending {
                    sqlx::query("DELETE FROM control_erase_jobs WHERE agent_ref=?")
                        .bind(&key)
                        .execute(&self.pool)
                        .await?;
                }
            }
        }
        Ok(count)
    }

    /// Record the native identity learned from retained vendor occurrences. The
    /// accepted target, submission and ingestion epoch remain immutable. Repeating
    /// the same binding returns its first receipt without permitting another launch.
    pub async fn bind_identity(
        &self,
        command_id: &str,
        agent_ref: AgentRef,
        source_evidence: Vec<ObservationRef>,
        at_ms: i64,
    ) -> Result<ObservationRef> {
        agent_ref
            .validate()
            .map_err(|error| OutboxError::Invalid(error.to_string()))?;
        if !matches!(agent_ref, AgentRef::Session(_)) || source_evidence.is_empty() {
            return Err(OutboxError::Invalid(
                "launch identity needs a native session and source evidence".into(),
            ));
        }
        for evidence in &source_evidence {
            evidence
                .validate()
                .map_err(|error| OutboxError::Invalid(error.to_string()))?;
            if evidence.issuer != Issuer::Environment(agent_ref.environment().clone()) {
                return Err(OutboxError::Invalid(
                    "identity evidence belongs to another environment".into(),
                ));
            }
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT * FROM trace_commands WHERE command_id=?")
            .bind(command_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(OutboxError::InvalidTransition)?;
        let command = self.command_row(&row)?;
        let CommandTarget::Launch { inventory } = &command.acceptance.target else {
            return Err(OutboxError::Invalid(
                "only a launch inventory can gain a native binding".into(),
            ));
        };
        if inventory.environment != *agent_ref.environment()
            || inventory.vendor != agent_ref.anchor_session().agent
        {
            return Err(OutboxError::Invalid(
                "native identity does not belong to the launch environment and vendor".into(),
            ));
        }
        if command.dispatch_attempts == 0 {
            return Err(OutboxError::InvalidTransition);
        }
        if let Some(bound) = &command.bound_agent {
            return if bound == &agent_ref {
                command
                    .identity_observation
                    .ok_or(OutboxError::InvalidTransition)
            } else {
                Err(OutboxError::IdentityConflict)
            };
        }
        let payload = serde_json::to_vec(&OperationalFact::IdentityBound {
            command_id: command_id.into(),
            agent_ref: agent_ref.clone(),
            source_evidence,
        })?;
        let prepared = self.prepare_payload(&payload)?;
        let occurrence = self
            .allocate(&mut tx, &command.acceptance, payload, prepared, at_ms)
            .await?;
        self.insert_occurrence(&mut tx, "identity_bound", &occurrence, at_ms)
            .await?;
        sqlx::query("UPDATE trace_commands SET bound_agent_json=?,bound_seq=? WHERE command_id=?")
            .bind(serde_json::to_vec(&agent_ref)?)
            .bind(occurrence.capture.observation_ref.producer_seq as i64)
            .bind(command_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(occurrence.capture.observation_ref)
    }

    /// The caller may invoke the vendor only after receiving this durable permit.
    pub async fn claim_dispatch(
        &self,
        command_id: &str,
        authorization: AuthorizationReceipt,
        at_ms: i64,
    ) -> Result<DispatchPermit> {
        self.claim_dispatch_inner(command_id, authorization, at_ms, None)
            .await
    }

    pub async fn claim_request_dispatch(
        &self,
        command_id: &str,
        authorization: AuthorizationReceipt,
        at_ms: i64,
        session_id: &SessionId,
        request: &sylph_protocol::Request,
    ) -> Result<DispatchPermit> {
        self.claim_dispatch_inner(
            command_id,
            authorization,
            at_ms,
            Some((
                session_id.encode(),
                request.id.clone(),
                serde_json::to_string(request)?,
            )),
        )
        .await
    }

    async fn claim_dispatch_inner(
        &self,
        command_id: &str,
        authorization: AuthorizationReceipt,
        at_ms: i64,
        request: Option<(String, String, String)>,
    ) -> Result<DispatchPermit> {
        if authorization.subject.is_empty() || authorization.decision_ref.is_empty() {
            return Err(OutboxError::Invalid(
                "dispatch requires a fresh authorization receipt".into(),
            ));
        }
        let mut command = self
            .command(command_id)
            .await?
            .ok_or(OutboxError::InvalidTransition)?;
        if authorization.subject != command.acceptance.authorization.subject {
            return Err(OutboxError::Invalid(
                "dispatch authorization belongs to a different subject".into(),
            ));
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row =
            sqlx::query("SELECT state, dispatch_attempts, bound_agent_json,current_input_json FROM trace_commands WHERE command_id = ?")
                .bind(command_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(OutboxError::InvalidTransition)?;
        let state = CommandState::from_db(row.get("state"))?;
        command.current_input = serde_json::from_slice(row.get::<&[u8], _>("current_input_json"))?;
        if row.get::<Option<&[u8]>, _>("bound_agent_json").is_some() {
            // A native identity proves the launch already took effect. An unknown
            // later input response must never authorize another launch, even when
            // the adapter supports idempotent requests on an existing session.
            return Err(OutboxError::InvalidTransition);
        }
        let retry_allowed = matches!(
            command.acceptance.retry_policy,
            RetryPolicy::VendorIdempotent { .. }
        );
        if state != CommandState::Accepted && !(state == CommandState::Unknown && retry_allowed) {
            return Err(OutboxError::InvalidTransition);
        }
        if (command.acceptance.kind == CommandKind::Approval) != request.is_some() {
            return Err(OutboxError::InvalidTransition);
        }
        if let Some((session_id, request_id, request_json)) = request {
            if command
                .acceptance
                .target
                .agent()
                .map(|agent| agent.anchor_session().encode())
                .as_deref()
                != Some(session_id.as_str())
            {
                return Err(OutboxError::InvalidTransition);
            }
            // Request ownership and the immutable dispatch fact share this FULL
            // transaction. A changed notice or another resolver cannot obtain a
            // second permit, even if both validated the old JSON beforehand.
            let claimed = sqlx::query("UPDATE current_requests SET state='resolving',resolution_command=? WHERE session_id=? AND request_id=? AND request_json=? AND state='waiting'")
                .bind(command_id).bind(session_id).bind(request_id).bind(request_json).execute(&mut *tx).await?;
            if claimed.rows_affected() != 1 {
                return Err(OutboxError::InvalidTransition);
            }
        }
        let attempt = row.get::<i64, _>("dispatch_attempts") as u64 + 1;
        let payload = serde_json::to_vec(&OperationalFact::Dispatched {
            command_id: command_id.into(),
            input_revision: command.current_input.revision.clone(),
            attempt,
            authorization,
        })?;
        let prepared = self.prepare_payload(&payload)?;
        let occurrence = self
            .allocate(&mut tx, &command.acceptance, payload, prepared, at_ms)
            .await?;
        self.insert_occurrence(&mut tx, &format!("dispatch:{attempt}"), &occurrence, at_ms)
            .await?;
        sqlx::query("UPDATE trace_commands SET state = 'dispatching', dispatch_attempts = ?,dispatched_revision=? WHERE command_id = ?")
            .bind(attempt as i64).bind(&command.current_input.revision).bind(command_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(DispatchPermit {
            command: CommandAcceptance {
                input: command.current_input,
                ..command.acceptance
            },
            attempt,
            observation_ref: occurrence.capture.observation_ref,
        })
    }

    /// The dispatch result remains authoritative after export and on status retries.
    pub async fn dispatch_result(&self, command_id: &str) -> Result<Option<DeliveryFact>> {
        let payload: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT payload FROM control_outbox WHERE command_id=? AND event_key='delivery:dispatch-result'",
        ).bind(command_id).fetch_optional(&self.readers).await?;
        payload.map(|payload| match serde_json::from_slice::<OperationalFact>(&payload)? {
            OperationalFact::Delivery { fact, .. } => Ok(fact),
            _ => Err(OutboxError::Invalid("dispatch result is not a delivery fact".into())),
        }).transpose()
    }

    pub async fn record_delivery(
        &self,
        command_id: &str,
        event_key: &str,
        fact: DeliveryFact,
        at_ms: i64,
    ) -> Result<ObservationRef> {
        if event_key.is_empty() {
            return Err(OutboxError::Invalid(
                "delivery occurrence key is empty".into(),
            ));
        }
        let mut command = self
            .command(command_id)
            .await?
            .ok_or(OutboxError::InvalidTransition)?;
        let payload = serde_json::to_vec(&OperationalFact::Delivery {
            command_id: command_id.into(),
            input_revision: command.current_input.revision.clone(),
            fact: fact.clone(),
        })?;
        if payload.len() > self.limits.max_fact_bytes {
            return Err(OutboxError::TooLarge);
        }
        let prepared = self.prepare_payload(&payload)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row =
            sqlx::query("SELECT state, dispatch_attempts FROM trace_commands WHERE command_id = ?")
                .bind(command_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(OutboxError::InvalidTransition)?;
        command.state = CommandState::from_db(row.get("state"))?;
        command.dispatch_attempts = row.get::<i64, _>("dispatch_attempts") as u64;
        let key = format!("delivery:{event_key}");
        if let Some(existing) = sqlx::query("SELECT observation_seq,payload FROM control_outbox WHERE command_id = ? AND event_key = ?")
            .bind(command_id).bind(&key).fetch_optional(&mut *tx).await?
        {
            if existing.get::<Vec<u8>, _>("payload") != payload { return Err(OutboxError::IdentityConflict); }
            return Ok(self.observation(existing.get::<i64, _>("observation_seq") as u64));
        }
        // Arrival order cannot turn contradictory receipts into a plausible winner.
        // Every occurrence is retained for reduction; this operational state only
        // prevents further dispatch while the contradictory claim is inspected.
        let state = match &fact {
            DeliveryFact::Cancelled { .. } if command.state == CommandState::Accepted => {
                "cancelled"
            }
            DeliveryFact::OutcomeUnknown { .. } if command.dispatch_attempts > 0 => {
                if command.state == CommandState::Dispatching {
                    "unknown"
                } else {
                    command.state.as_db()
                }
            }
            DeliveryFact::ReceiverAccepted { .. } if command.dispatch_attempts > 0 => {
                match command.state {
                    CommandState::Rejected | CommandState::Conflicting => "conflicting",
                    CommandState::Consumed => "consumed",
                    _ => "receiver_accepted",
                }
            }
            DeliveryFact::Rejected { .. } if command.dispatch_attempts > 0 => match command.state {
                CommandState::Consumed
                | CommandState::ReceiverAccepted
                | CommandState::Conflicting => "conflicting",
                _ => "rejected",
            },
            DeliveryFact::Consumed { revision, .. }
                if command.dispatch_attempts > 0 && revision == &command.current_input.revision =>
            {
                if matches!(
                    command.state,
                    CommandState::Rejected | CommandState::Conflicting
                ) {
                    "conflicting"
                } else {
                    "consumed"
                }
            }
            _ => return Err(OutboxError::InvalidTransition),
        };
        let occurrence = self
            .allocate(&mut tx, &command.acceptance, payload, prepared, at_ms)
            .await?;
        self.insert_occurrence(&mut tx, &key, &occurrence, at_ms)
            .await?;
        if command.acceptance.kind == CommandKind::Approval {
            match state {
                "rejected" => {
                    sqlx::query("UPDATE current_requests SET state='waiting',resolution_command=NULL WHERE resolution_command=? AND state='resolving'")
                        .bind(command_id).execute(&mut *tx).await?;
                }
                "receiver_accepted" | "consumed" => {
                    sqlx::query(
                        "UPDATE current_requests SET state='resolved' WHERE resolution_command=?",
                    )
                    .bind(command_id)
                    .execute(&mut *tx)
                    .await?;
                }
                _ => {}
            }
        }
        sqlx::query("UPDATE trace_commands SET state = ? WHERE command_id = ?")
            .bind(state)
            .bind(command_id)
            .execute(&mut *tx)
            .await?;
        #[cfg(test)]
        kill_at("before_result_commit");
        tx.commit().await?;
        #[cfg(test)]
        kill_at("after_result_commit");
        Ok(occurrence.capture.observation_ref)
    }

    /// Run before starting dispatch workers after restart. Accepted commands remain
    /// safe to dispatch; an attempt whose response was lost becomes unknown.
    pub async fn recover_inflight(&self, at_ms: i64) -> Result<usize> {
        let ids: Vec<(String, i64)> = sqlx::query_as("SELECT command_id,dispatch_attempts FROM trace_commands WHERE state = 'dispatching' ORDER BY created_at_ms,command_id LIMIT ?")
            .bind(self.limits.max_export_items as i64).fetch_all(&self.readers).await?;
        for (id, attempt) in &ids {
            self.record_delivery(
                id,
                &format!("restart:{attempt}"),
                DeliveryFact::OutcomeUnknown {
                    reason: "Server restarted before the vendor response was durably recorded"
                        .into(),
                },
                at_ms,
            )
            .await?;
        }
        Ok(ids.len())
    }

    fn prepare_payload(&self, payload: &[u8]) -> Result<PreparedPayload> {
        if payload.len() > self.limits.max_fact_bytes {
            return Err(OutboxError::TooLarge);
        }
        let digest = |bytes: &[u8]| {
            PayloadRef::bytes(blake3::hash(bytes).to_hex())
                .map_err(|error| OutboxError::Invalid(error.to_string()))
        };
        if payload.len() <= self.limits.payload_chunk_bytes {
            return Ok(PreparedPayload {
                reference: digest(payload)?,
                manifest: None,
            });
        }
        if payload.len().div_ceil(self.limits.payload_chunk_bytes) > self.limits.max_payload_chunks
        {
            return Err(OutboxError::TooLarge);
        }
        let chunks = payload
            .chunks(self.limits.payload_chunk_bytes)
            .enumerate()
            .map(|(index, bytes)| {
                Ok(PayloadChunk {
                    digest: digest(bytes)?,
                    chunk_offset: 0,
                    length: bytes.len() as u64,
                    payload_offset: (index * self.limits.payload_chunk_bytes) as u64,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let manifest = PayloadManifest {
            chunks,
            media_type: "application/json".into(),
            encoding: PayloadEncoding::Utf8,
            decoded_length: Some(payload.len() as u64),
            completeness: PayloadCompleteness::Complete,
        };
        if serde_json::to_vec(&manifest)?.len() > self.limits.max_manifest_bytes {
            return Err(OutboxError::TooLarge);
        }
        let reference = crate::trace::evidence::manifest_ref(&manifest)
            .map_err(|error| OutboxError::Invalid(error.to_string()))?;
        Ok(PreparedPayload {
            reference,
            manifest: Some(manifest),
        })
    }

    async fn allocate(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        command: &CommandAcceptance,
        payload: Vec<u8>,
        prepared: PreparedPayload,
        at_ms: i64,
    ) -> Result<OutboxOccurrence> {
        if payload.len() > self.limits.max_fact_bytes {
            return Err(OutboxError::TooLarge);
        }
        let row = sqlx::query("UPDATE control_producer SET next_seq = next_seq + 1 WHERE singleton = 1 AND next_seq < 9223372036854775807 RETURNING next_seq")
            .fetch_optional(&mut **tx).await?.ok_or(OutboxError::Capacity)?;
        let seq = row.get::<i64, _>("next_seq") as u64;
        let capture = Capture {
            observation_ref: self.observation(seq),
            source_position: SourcePosition {
                source: SourceRef {
                    issuer: self.issuer.clone(),
                    instance: self.producer_id.clone(),
                },
                generation: 0,
                locator: SourceLocator::Occurrence {
                    key: seq.to_string(),
                },
            },
            ingestion_authorization_epoch: command.ingestion_authorization_epoch.clone(),
            declared_subject_refs: command.target.declared_subjects(),
            captured_at: TimeEvidence {
                domain: format!("server-control:{}", self.producer_id),
                value: at_ms,
                unit: TimeUnit::Milliseconds,
                precision: Some(1),
                basis: TimeBasis::ServerReceipt,
            },
            source_clock: None,
            transport_kind: TransportKind::Control,
            vendor_build: None,
            schema_fingerprint: Some("sylph-control-fact".into()),
            payload_ref: prepared.reference,
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        Ok(OutboxOccurrence {
            command_id: command.command_id.clone(),
            capture,
            payload,
            manifest: prepared.manifest,
        })
    }

    async fn insert_occurrence(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        event_key: &str,
        occurrence: &OutboxOccurrence,
        at_ms: i64,
    ) -> Result<()> {
        let capture = serde_json::to_vec(&occurrence.capture)?;
        if capture.len() > self.limits.max_capture_envelope_bytes {
            return Err(OutboxError::TooLarge);
        }
        let manifest = occurrence
            .manifest
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()?;
        let bytes = capture
            .len()
            .checked_add(occurrence.payload.len())
            .and_then(|bytes| bytes.checked_add(manifest.as_ref().map_or(0, Vec::len)))
            .ok_or(OutboxError::TooLarge)?;
        if bytes > self.limits.max_fact_bytes {
            return Err(OutboxError::TooLarge);
        }
        let updated = sqlx::query("UPDATE control_producer SET pending_count = pending_count + 1, pending_bytes = pending_bytes + ? WHERE singleton = 1 AND pending_count < ? AND pending_bytes <= ?")
            .bind(bytes as i64).bind(self.limits.max_pending_facts as i64)
            .bind(self.limits.max_pending_bytes.saturating_sub(bytes as u64) as i64)
            .execute(&mut **tx).await?;
        if updated.rows_affected() != 1 {
            return Err(OutboxError::Capacity);
        }
        sqlx::query("INSERT INTO control_outbox (observation_seq,command_id,event_key,capture_json,payload,manifest_json,encoded_bytes,created_at_ms) VALUES (?,?,?,?,?,?,?,?)")
            .bind(occurrence.capture.observation_ref.producer_seq as i64).bind(&occurrence.command_id)
            .bind(event_key).bind(capture).bind(&occurrence.payload).bind(manifest).bind(bytes as i64).bind(at_ms)
            .execute(&mut **tx).await?;
        Ok(())
    }

    /// Metadata is selected first; payload reads happen only within both bounds.
    pub async fn pending(&self) -> Result<Vec<OutboxOccurrence>> {
        let rows: Vec<(i64, i64)> = sqlx::query_as("SELECT observation_seq,encoded_bytes FROM control_outbox WHERE exported_disposition IS NULL ORDER BY observation_seq LIMIT ?")
            .bind(self.limits.max_export_items as i64).fetch_all(&self.readers).await?;
        let mut used = 0usize;
        let mut result = Vec::new();
        for (seq, bytes) in rows {
            let bytes = usize::try_from(bytes).map_err(|_| OutboxError::TooLarge)?;
            let Some(next) = used.checked_add(bytes) else {
                if result.is_empty() {
                    return Err(OutboxError::TooLarge);
                }
                break;
            };
            if next > self.limits.max_export_bytes {
                // Lowering a limit cannot make retained work look fully exported.
                // The caller must expose the bound and adjust configuration; a
                // nonempty batch can still advance before its next larger item.
                if result.is_empty() {
                    return Err(OutboxError::TooLarge);
                }
                break;
            }
            let row = sqlx::query("SELECT command_id,capture_json,payload,manifest_json FROM control_outbox WHERE observation_seq = ?")
                .bind(seq).fetch_one(&self.readers).await?;
            result.push(OutboxOccurrence {
                command_id: row.get("command_id"),
                capture: serde_json::from_slice(row.get::<&[u8], _>("capture_json"))?,
                payload: row.get("payload"),
                manifest: row
                    .get::<Option<&[u8]>, _>("manifest_json")
                    .map(serde_json::from_slice)
                    .transpose()?,
            });
            used = next;
        }
        Ok(result)
    }

    /// A terminal deletion receipt releases producer work without claiming admission.
    /// Neither disposition changes command state or authorizes another dispatch.
    pub async fn mark_exported(
        &self,
        observation: &ObservationRef,
        disposition: ExportDisposition,
    ) -> Result<()> {
        if observation.issuer != self.issuer
            || observation.producer_id != self.producer_id
            || observation.producer_epoch != self.producer_epoch
            || observation.producer_seq > i64::MAX as u64
        {
            return Err(OutboxError::ReceiptMismatch);
        }
        let (name, cut) = match disposition {
            ExportDisposition::Retained { admission_cut } if admission_cut <= i64::MAX as u64 => {
                ("retained", Some(admission_cut as i64))
            }
            ExportDisposition::DiscardedByDeletion => ("discarded_by_deletion", None),
            _ => return Err(OutboxError::ReceiptMismatch),
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT encoded_bytes,exported_disposition,admission_cut FROM control_outbox WHERE observation_seq = ?")
            .bind(observation.producer_seq as i64).fetch_optional(&mut *tx).await?.ok_or(OutboxError::ReceiptMismatch)?;
        if let Some(existing) = row.get::<Option<String>, _>("exported_disposition") {
            return if existing == name && row.get::<Option<i64>, _>("admission_cut") == cut {
                Ok(())
            } else {
                Err(OutboxError::ReceiptMismatch)
            };
        }
        sqlx::query("UPDATE control_outbox SET exported_disposition = ?, admission_cut = ? WHERE observation_seq = ?")
            .bind(name).bind(cut).bind(observation.producer_seq as i64).execute(&mut *tx).await?;
        sqlx::query("UPDATE control_producer SET pending_count = pending_count - 1, pending_bytes = pending_bytes - ? WHERE singleton = 1")
            .bind(row.get::<i64, _>("encoded_bytes")).execute(&mut *tx).await?;
        #[cfg(test)]
        kill_at("before_export_mark_commit");
        tx.commit().await?;
        #[cfg(test)]
        kill_at("after_export_mark_commit");
        Ok(())
    }

    pub async fn progress(&self, now_ms: i64) -> Result<OutboxProgress> {
        let row = sqlx::query("SELECT pending_count,pending_bytes,(SELECT MIN(created_at_ms) FROM control_outbox WHERE exported_disposition IS NULL) AS oldest FROM control_producer WHERE singleton = 1")
            .fetch_one(&self.readers).await?;
        Ok(OutboxProgress {
            pending_count: row.get::<i64, _>("pending_count") as u64,
            pending_bytes: row.get::<i64, _>("pending_bytes") as u64,
            oldest_pending_age_ms: row
                .get::<Option<i64>, _>("oldest")
                .map(|at| now_ms.saturating_sub(at).max(0) as u64),
        })
    }
}

#[cfg(test)]
mod tests;

// Hooks exist only in test binaries. The fixture marker and selected point are
// set exclusively on a spawned probe; the suite process cannot kill itself.
#[cfg(test)]
pub(crate) fn kill_at(point: &str) {
    #[cfg(unix)]
    if std::env::var_os("SYLPH_OUTBOX_CRASH_FIXTURE").is_some()
        && std::env::var("SYLPH_OUTBOX_CRASH_POINT").as_deref() == Ok(point)
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // SAFETY: SIGKILL targets only the isolated probe's own process.
        unsafe { kill(std::process::id() as i32, 9) };
        unreachable!("SIGKILL must terminate the outbox probe");
    }
    let _ = point;
}
