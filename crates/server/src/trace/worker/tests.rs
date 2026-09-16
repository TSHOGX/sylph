use super::*;
use crate::{
    trace::cas::BlobStore,
    trace::{
        evidence::{
            catalogue::{BuildTicket, CatalogueLimits},
            AdmissionLimits,
        },
        generation::{GenerationManifest, QueryLimits},
    },
};
use sqlx::{Connection, Row};
use sylph_protocol::{AgentName, EnvironmentId, SessionId};

const CODEX_NATIVE: &[u8] =
    include_bytes!("../../../../../fixtures/native/codex/user-input-response-item.jsonl");

struct Fixture {
    directory: tempfile::TempDir,
    evidence: EvidenceStore,
    catalogue: EvidenceCatalogue,
    scope: IngestionScope,
    epoch: IngestionEpoch,
}

impl Fixture {
    async fn new() -> Self {
        Self::for_vendor("codex").await
    }
    async fn for_vendor(vendor: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let evidence = EvidenceStore::open(
            directory.path().join("evidence.db"),
            BlobStore::new(directory.path().join("blobs")),
            AdmissionLimits::default(),
        )
        .await
        .unwrap();
        let scope = IngestionScope::Inventory(InventoryRef {
            environment: EnvironmentId::parse("env7fk2").unwrap(),
            vendor: AgentName::parse(vendor).unwrap(),
            instance: "fixture-launch".into(),
        });
        let epoch = evidence.authorize_scope(&scope, &[]).await.unwrap();
        let catalogue =
            EvidenceCatalogue::new(evidence.clone(), CatalogueLimits::default()).unwrap();
        Self {
            directory,
            evidence,
            catalogue,
            scope,
            epoch,
        }
    }
    fn worker(&self) -> ProjectionWorker {
        self.worker_with(WorkerLimits::default())
    }
    fn worker_with(&self, limits: WorkerLimits) -> ProjectionWorker {
        ProjectionWorker::new(self.evidence.clone(), self.catalogue.clone(), limits).unwrap()
    }
    async fn generation(&self, name: &str) -> (GenerationStore, BuildTicket) {
        self.generation_with_limits(name, QueryLimits::default())
            .await
    }
    async fn generation_with_limits(
        &self,
        name: &str,
        limits: QueryLimits,
    ) -> (GenerationStore, BuildTicket) {
        let manifest = GenerationManifest {
            generation: name.into(),
            semantic_schema: "trace-1".into(),
            mapper_builds: BTreeMap::from([
                ("codex".into(), mapping::codex::CodexMapper.build().into()),
                (
                    "server".into(),
                    mapping::server::ServerMapper.build().into(),
                ),
                (
                    "claude".into(),
                    mapping::claude::ClaudeMapper.build().into(),
                ),
                ("pi".into(), mapping::pi::PiMapper.build().into()),
                (
                    "attachment".into(),
                    mapping::attachment::AttachmentMapper.build().into(),
                ),
                (
                    "inventory".into(),
                    mapping::inventory::InventoryMapper.build().into(),
                ),
            ]),
            reducer_build: "reducer-1".into(),
            capture_inventory_build: "inventory-1".into(),
            encoding_build: "encoding-1".into(),
        };
        let ticket = self
            .catalogue
            .begin_build(manifest.clone(), 10)
            .await
            .unwrap();
        let store = GenerationStore::create(
            self.directory.path().join(&ticket.relative_file),
            manifest,
            limits,
        )
        .await
        .unwrap();
        let validation = store.validate_for_publication().await.unwrap();
        self.catalogue
            .register_file(&ticket, &validation.database_identity)
            .await
            .unwrap();
        (store, ticket)
    }
    async fn stage(&self, raw: &[u8]) -> PayloadRef {
        let limit = self.evidence.limits().max_chunk_bytes;
        if raw.len() <= limit {
            return self.evidence.stage_payload(raw).await.unwrap();
        }
        let mut chunks = Vec::new();
        for (index, bytes) in raw.chunks(limit).enumerate() {
            chunks.push(PayloadChunk {
                digest: self.evidence.stage_payload(bytes).await.unwrap(),
                chunk_offset: 0,
                length: bytes.len() as u64,
                payload_offset: (index * limit) as u64,
            });
        }
        self.evidence
            .stage_manifest(&PayloadManifest {
                chunks,
                media_type: "application/x-ndjson".into(),
                encoding: PayloadEncoding::Binary,
                decoded_length: Some(raw.len() as u64),
                completeness: PayloadCompleteness::Complete,
            })
            .await
            .unwrap()
    }
    async fn admit(
        &self,
        reference: ObservationRef,
        position: SourcePosition,
        raw: &[u8],
        kind: TransportKind,
    ) -> Capture {
        let capture = Capture {
            observation_ref: reference,
            source_position: position,
            ingestion_authorization_epoch: self.epoch.clone(),
            declared_subject_refs: vec![],
            captured_at: TimeEvidence {
                domain: "fixture.wall".into(),
                value: 100,
                unit: TimeUnit::Milliseconds,
                precision: Some(1),
                basis: TimeBasis::SourceWall,
            },
            source_clock: None,
            transport_kind: kind,
            vendor_build: Some("fixture".into()),
            schema_fingerprint: None,
            payload_ref: self.stage(raw).await,
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        self.evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        capture
    }
    async fn native(&self, sequence: u64, offset: u64, raw: &[u8]) -> Capture {
        let observation = native_observation(sequence);
        let position = SourcePosition {
            source: SourceRef {
                issuer: observation.issuer.clone(),
                instance: "native-file".into(),
            },
            generation: 0,
            locator: SourceLocator::Jsonl {
                byte_start: offset,
                byte_end: offset + raw.len() as u64,
                record_ordinal: sequence - 1,
            },
        };
        self.admit(observation, position, raw, TransportKind::NativeJsonl)
            .await
    }
    async fn native_fixture(&self) -> Vec<Capture> {
        let mut offset = 0;
        let mut captures = Vec::new();
        for (index, line) in CODEX_NATIVE
            .split_inclusive(|byte| *byte == b'\n')
            .enumerate()
        {
            captures.push(self.native(index as u64 + 1, offset, line).await);
            offset += line.len() as u64;
        }
        captures
    }
    async fn control(&self, sequence: u64, fact: OperationalFact) -> Capture {
        let observation = ObservationRef {
            issuer: Issuer::Server("control-server".into()),
            producer_id: "controls".into(),
            producer_epoch: "one".into(),
            producer_seq: sequence,
        };
        let position = SourcePosition {
            source: SourceRef {
                issuer: observation.issuer.clone(),
                instance: "outbox".into(),
            },
            generation: 0,
            locator: SourceLocator::Occurrence {
                key: sequence.to_string(),
            },
        };
        self.admit(
            observation,
            position,
            &serde_json::to_vec(&fact).unwrap(),
            TransportKind::Control,
        )
        .await
    }
    fn accepted(&self) -> OperationalFact {
        let IngestionScope::Inventory(inventory) = &self.scope else {
            unreachable!()
        };
        OperationalFact::Accepted {
            command: Box::new(CommandAcceptance {
                command_id: "launch-command".into(),
                target: CommandTarget::Launch {
                    inventory: inventory.clone(),
                },
                kind: CommandKind::Send,
                input: SubmittedRevision {
                    revision: "submitted-v1".into(),
                    media_type: "application/octet-stream".into(),
                    bytes: b"Exact human \0\xff\r\n".to_vec(),
                },
                authorization: AuthorizationReceipt {
                    subject: "person-one".into(),
                    decision_ref: "grant".into(),
                    lease_ref: None,
                    evaluated_at_ms: 1,
                },
                ingestion_authorization_epoch: self.epoch.clone(),
                retry_policy: RetryPolicy::AtMostOnce,
                idempotency_key: None,
                accepted_at_ms: 2,
            }),
        }
    }
}

fn native_observation(sequence: u64) -> ObservationRef {
    ObservationRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
        producer_id: "native".into(),
        producer_epoch: "one".into(),
        producer_seq: sequence,
    }
}
fn agent() -> AgentRef {
    AgentRef::Session(SessionId::decode("env7fk2:codex:response-user").unwrap())
}
fn message(id: &str) -> MessageRef {
    MessageRef::native(agent(), "codex.message", vec![id.into()]).unwrap()
}
async fn claims(store: &GenerationStore) -> Vec<Claim> {
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let mut result = Vec::new();
    let mut after = None;
    loop {
        let page = store
            .retained_claims(
                &snapshot,
                after.as_ref(),
                store.limits().max_candidates,
                store.limits().max_batch_bytes,
            )
            .await
            .unwrap();
        result.extend(page.claims);
        if !page.has_more {
            break;
        }
        after = page.next;
    }
    result
}
/// Every reader row visible at the current snapshot, keyed by its served scope and
/// address. Claim equality alone cannot catch a reader-materialization defect: the
/// rows are a separate derived surface, and incremental scheduling must not change
/// which ones a page returns or what body each carries.
async fn reader_rows(store: &GenerationStore) -> BTreeMap<(String, String), serde_json::Value> {
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let (mut conn, _) = store.read_connection(&snapshot).await.unwrap();
    let revision = snapshot.view_revision as i64;
    sqlx::query(
        "SELECT scope_key,address,body_json FROM read_rows \
         WHERE present=1 AND valid_from<=? AND (valid_to IS NULL OR valid_to>?) \
         ORDER BY scope_key,address",
    )
    .bind(revision)
    .bind(revision)
    .fetch_all(&mut *conn)
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        (
            (row.get("scope_key"), row.get("address")),
            serde_json::from_str(&row.get::<String, _>("body_json")).unwrap(),
        )
    })
    .collect()
}
async fn revision_bytes(
    fixture: &Fixture,
    store: &GenerationStore,
    message: &MessageRef,
) -> Vec<Vec<u8>> {
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let mut bodies = Vec::new();
    for claim in claims(store).await {
        if let Fact::Declare {
            object: TraceObject::MessageRevision(revision),
        } = claim.fact
        {
            if &revision.message != message {
                continue;
            }
            let detail = store
                .object(
                    &snapshot,
                    &AnyObjectRef::MessageRevision(revision.revision_ref),
                )
                .await
                .unwrap();
            let TraceObject::MessageRevision(revision) = detail.object else {
                panic!("revision object")
            };
            let mut body = Vec::new();
            for part in revision.parts {
                body.extend(
                    fixture
                        .evidence
                        .payload_bytes(&part.payload, 4 * mapping::DERIVED_PAYLOAD_CHUNK_BYTES)
                        .await
                        .unwrap(),
                );
            }
            bodies.push(body);
        }
    }
    bodies
}
async fn assert_native_values(fixture: &Fixture, store: &GenerationStore) {
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let detail = store
        .object(&snapshot, &AnyObjectRef::Message(message("answer-1")))
        .await
        .unwrap();
    let TraceObject::Message(answer) = detail.object else {
        panic!("assistant message")
    };
    assert_eq!(
        answer.author,
        Author::Agent {
            agent: OwnerRef::Agent { agent: agent() }
        }
    );
    assert_eq!(
        answer.production.unwrap().owner,
        OwnerRef::Agent { agent: agent() }
    );
    assert_eq!(answer.purpose, MessagePurpose::Answer);
    assert_eq!(
        revision_bytes(fixture, store, &message("answer-1")).await,
        vec![b"The transcript is available.".to_vec()]
    );
    assert_eq!(
        revision_bytes(fixture, store, &message("ask-1")).await,
        vec![b"Recover the missing transcript".to_vec()]
    );
    let page = store
        .page(&snapshot, &QueryScope::archive(agent()), None, 10)
        .await
        .unwrap();
    assert_eq!(page.objects.len(), 2);
}

#[tokio::test]
async fn cold_and_restarted_incremental_projection_preserve_fixture_owner_and_exact_bodies() {
    let fixture = Fixture::new().await;
    fixture.native_fixture().await;
    let (cold, _) = fixture.generation("cold").await;
    fixture.worker().catch_up(&cold, 1, 5).await.unwrap();
    assert_native_values(&fixture, &cold).await;
    let (mut incremental, _) = fixture.generation("incremental").await;
    fixture.worker().catch_up(&incremental, 1, 3).await.unwrap();
    let old = incremental.checkpoint().await.unwrap().snapshot;
    let path = incremental.path().to_path_buf();
    let manifest = incremental.manifest().clone();
    let limits = incremental.limits().clone();
    incremental.close().await;
    incremental = GenerationStore::open(path, manifest, limits).await.unwrap();
    fixture.worker().catch_up(&incremental, 1, 5).await.unwrap();
    assert_native_values(&fixture, &incremental).await;
    assert_eq!(claims(&cold).await, claims(&incremental).await);
    assert_eq!(reader_rows(&cold).await, reader_rows(&incremental).await);
    assert_eq!(
        incremental
            .page(&old, &QueryScope::archive(agent()), None, 10)
            .await
            .unwrap()
            .objects
            .len(),
        1
    );
}

#[tokio::test]
async fn new_receipt_for_a_retained_source_occurrence_does_not_duplicate_projection() {
    let fixture = Fixture::new().await;
    let captures = fixture.native_fixture().await;
    let (generation, _) = fixture.generation("reread").await;
    let worker = fixture.worker();
    worker.catch_up(&generation, 1, 5).await.unwrap();
    let before = claims(&generation).await;
    let original = &captures[2];
    let mut repeated = original.clone();
    repeated.observation_ref.producer_seq = 6;
    repeated.captured_at.value = 200;
    fixture
        .evidence
        .admit_batch(
            &repeated.observation_ref.issuer,
            std::slice::from_ref(&repeated),
        )
        .await
        .unwrap();
    worker.catch_up(&generation, 1, 6).await.unwrap();
    assert_eq!(claims(&generation).await, before);
    assert_native_values(&fixture, &generation).await;
    assert_eq!(
        generation.checkpoint().await.unwrap().snapshot.evidence_cut,
        6
    );
}

#[tokio::test]
async fn derived_text_larger_than_one_chunk_is_retrievable_byte_for_byte() {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let text = "z".repeat(mapping::DERIVED_PAYLOAD_CHUNK_BYTES + 317);
    let mut raw=serde_json::to_vec(&serde_json::json!({"type":"response_item","payload":{"type":"message","id":"large-answer","role":"assistant","content":[{"type":"output_text","text":text}]}})).unwrap();
    raw.push(b'\n');
    fixture.native(2, header.len() as u64, &raw).await;
    let (generation, _) = fixture.generation("large").await;
    fixture.worker().catch_up(&generation, 1, 2).await.unwrap();
    assert_eq!(
        revision_bytes(&fixture, &generation, &message("large-answer")).await,
        vec![text.into_bytes()]
    );
    let revision = claims(&generation)
        .await
        .into_iter()
        .find_map(|claim| match claim.fact {
            Fact::Declare {
                object: TraceObject::MessageRevision(value),
            } => Some(value),
            _ => None,
        })
        .unwrap();
    assert_eq!(revision.parts[0].payload.kind(), PayloadKind::Manifest);
    assert_eq!(
        fixture
            .evidence
            .payload_manifest(&revision.parts[0].payload)
            .await
            .unwrap()
            .unwrap()
            .chunks
            .len(),
        2
    );
}

#[tokio::test]
async fn failed_projection_commit_keeps_evidence_and_checkpoint_retryable() {
    let fixture = Fixture::new().await;
    fixture.native_fixture().await;
    let (generation, _) = fixture.generation("failure").await;
    let worker = fixture.worker();
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(generation.path()),
    )
    .await
    .unwrap();
    // Fail after claim and object writes have begun, so the assertions verify
    // rollback of an entire projection unit rather than refusal before any write.
    sqlx::raw_sql("CREATE TRIGGER reject_projection BEFORE INSERT ON mapper_states BEGIN SELECT RAISE(ABORT,'projection failure'); END;").execute(&mut connection).await.unwrap();
    assert!(worker.catch_up(&generation, 1, 5).await.is_err());
    assert_eq!(
        generation.checkpoint().await.unwrap().snapshot.evidence_cut,
        0
    );
    assert!(claims(&generation).await.is_empty());
    assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), 5);
    assert!(matches!(
        fixture
            .evidence
            .receipt(&native_observation(5))
            .await
            .unwrap()
            .unwrap()
            .disposition,
        ReceiptDisposition::Retained { .. }
    ));
    sqlx::query("DROP TRIGGER reject_projection")
        .execute(&mut connection)
        .await
        .unwrap();
    worker.catch_up(&generation, 1, 5).await.unwrap();
    assert_native_values(&fixture, &generation).await;
    let (reference, _) = fixture.generation("failure-reference").await;
    fixture.worker().catch_up(&reference, 1, 5).await.unwrap();
    assert_eq!(claims(&generation).await, claims(&reference).await);
}

#[tokio::test]
async fn cancellation_after_payload_staging_cannot_publish_uncommitted_mapper_state() {
    let fixture = Fixture::new().await;
    fixture.native_fixture().await;
    let (generation, _) = fixture.generation("cancel").await;
    let worker = fixture.worker();
    worker.catch_up(&generation, 1, 2).await.unwrap();
    // Keep publication blocked after independent evidence payload staging. This
    // window does not depend on which mapping/reduction work uses the CPU pool.
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(generation.path()),
    )
    .await
    .unwrap();
    let transaction = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let running = {
        let worker = worker.clone();
        let generation = generation.clone();
        tokio::spawn(async move { worker.catch_up(&generation, 1, 5).await })
    };
    let expected =
        PayloadRef::bytes(blake3::hash(b"Recover the missing transcript").to_hex()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if fixture
                .evidence
                .payload_bytes(&expected, 1024)
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    transaction.rollback().await.unwrap();
    assert_eq!(
        generation.checkpoint().await.unwrap().snapshot.evidence_cut,
        2
    );
    worker.catch_up(&generation, 1, 5).await.unwrap();
    assert_native_values(&fixture, &generation).await;
    let (reference, _) = fixture.generation("cancellation-reference").await;
    fixture.worker().catch_up(&reference, 1, 5).await.unwrap();
    assert_eq!(claims(&generation).await, claims(&reference).await);
}

fn binding(proof: ObservationRef) -> OperationalFact {
    OperationalFact::IdentityBound {
        command_id: "launch-command".into(),
        agent_ref: agent(),
        source_evidence: vec![proof],
    }
}
fn submitted_message() -> MessageRef {
    MessageRef::native(
        agent(),
        "sylph.command.message",
        vec!["launch-command".into()],
    )
    .unwrap()
}
async fn assert_submitted(fixture: &Fixture, generation: &GenerationStore) {
    let snapshot = generation.checkpoint().await.unwrap().snapshot;
    let detail = generation
        .object(&snapshot, &AnyObjectRef::Message(submitted_message()))
        .await
        .unwrap();
    let TraceObject::Message(value) = detail.object else {
        panic!("human submission")
    };
    assert_eq!(
        value.author,
        Author::Human {
            subject: Some("person-one".into())
        }
    );
    assert_eq!(value.message_ref.scope(), &agent());
    assert_eq!(
        revision_bytes(fixture, generation, &submitted_message()).await,
        vec![b"Exact human \0\xff\r\n".to_vec()]
    );
    assert!(!claims(generation).await.iter().any(|claim| matches!(
        claim.fact,
        Fact::Declare {
            object: TraceObject::Activation(_)
        }
    )));
}

#[tokio::test]
async fn launch_acceptance_waits_for_native_identity_without_opening_an_activation() {
    let fixture = Fixture::new().await;
    fixture.control(1, fixture.accepted()).await;
    let (generation, _) = fixture.generation("launch").await;
    let worker = fixture.worker();
    let pending = worker.catch_up(&generation, 1, 1).await.unwrap();
    assert_eq!(pending.pending_mappings, 1);
    assert!(claims(&generation).await.is_empty());
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    fixture.control(2, binding(native_observation(1))).await;
    let complete = worker.catch_up(&generation, 1, 3).await.unwrap();
    assert_eq!(complete.pending_mappings, 0);
    assert_submitted(&fixture, &generation).await;
}

#[tokio::test]
async fn missing_identity_proof_survives_restart_and_resolves_without_another_admission() {
    let fixture = Fixture::new().await;
    fixture.control(1, fixture.accepted()).await;
    fixture.control(2, binding(native_observation(1))).await;
    let (generation, _) = fixture.generation("late-proof").await;
    let pending = fixture.worker().catch_up(&generation, 1, 2).await.unwrap();
    assert!(pending.pending_mappings > 0);
    assert!(claims(&generation).await.is_empty());
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let resolved = fixture.worker().catch_up(&generation, 1, 3).await.unwrap();
    assert_eq!(resolved.pending_mappings, 0);
    assert!(generation
        .mapper_states()
        .await
        .unwrap()
        .iter()
        .filter(|(key, _)| key.starts_with("diagnostic:"))
        .all(|(_, value)| value.is_null()));
    assert_submitted(&fixture, &generation).await;
}

#[tokio::test]
async fn native_identity_binding_links_pre_identity_evidence_to_the_agent_deletion_scope() {
    let fixture = Fixture::new().await;
    let accepted = fixture.control(1, fixture.accepted()).await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    fixture.control(2, binding(native_observation(1))).await;
    let (generation, _) = fixture.generation("binding-deletion").await;
    fixture.worker().catch_up(&generation, 1, 3).await.unwrap();
    fixture
        .evidence
        .delete_scope(&IngestionScope::Agent(agent()))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&accepted.observation_ref, 1024 * 1024)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn missing_native_identity_proof_keeps_submission_unprojected_despite_matching_agent_metadata(
) {
    let fixture = Fixture::new().await;
    fixture.control(1, fixture.accepted()).await;
    fixture.control(2, binding(native_observation(1))).await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    let observation = ObservationRef {
        producer_id: "unrelated-native".into(),
        ..native_observation(1)
    };
    let position = SourcePosition {
        source: SourceRef {
            issuer: observation.issuer.clone(),
            instance: "unrelated-header".into(),
        },
        generation: 0,
        locator: SourceLocator::Jsonl {
            byte_start: 0,
            byte_end: header.len() as u64,
            record_ordinal: 0,
        },
    };
    fixture
        .admit(observation, position, header, TransportKind::NativeJsonl)
        .await;
    let (generation, _) = fixture.generation("unresolved-proof").await;
    let worker = fixture.worker();
    let progress = worker.catch_up(&generation, 1, 3).await.unwrap();
    assert_eq!(progress.pending_mappings, 2);
    assert!(!claims(&generation).await.iter().any(|claim| matches!(
        claim.fact,
        Fact::Declare {
            object: TraceObject::Message(_) | TraceObject::Activation(_)
        }
    )));
    let repeated = worker.catch_up(&generation, 1, 3).await.unwrap();
    assert_eq!(repeated.pending_mappings, 2);
    assert_eq!(repeated.projected_cut, 3);
}

#[tokio::test]
async fn derived_payload_provenance_survives_projection_restart_with_exact_source_selector() {
    let fixture = Fixture::new().await;
    let captures = fixture.native_fixture().await;
    let (generation, _) = fixture.generation("provenance").await;
    fixture.worker().catch_up(&generation, 1, 5).await.unwrap();
    let payload =
        PayloadRef::bytes(blake3::hash(b"Recover the missing transcript").to_hex()).unwrap();
    let snapshot = generation.checkpoint().await.unwrap().snapshot;
    let derivations = generation
        .payload_derivations(&snapshot, &payload, 10)
        .await
        .unwrap();
    assert_eq!(derivations.len(), 1);
    assert_eq!(derivations[0].payload, payload);
    assert_eq!(
        derivations[0].evidence,
        vec![captures[2].observation_ref.clone()]
    );
    assert_eq!(derivations[0].selector, "/payload/content/0/text");
    assert_eq!(derivations[0].rule, "codex/json-field-decoding");
    assert_eq!(derivations[0].media_type, "text/plain");
    assert_eq!(derivations[0].encoding, PayloadEncoding::Utf8);
    let path = generation.path().to_path_buf();
    let manifest = generation.manifest().clone();
    let limits = generation.limits().clone();
    generation.close().await;
    let reopened = GenerationStore::open(path, manifest, limits).await.unwrap();
    assert_eq!(
        reopened
            .payload_derivations(&snapshot, &payload, 10)
            .await
            .unwrap(),
        derivations
    );
}

#[tokio::test]
async fn publication_validation_rejects_a_missing_derived_chunk_without_changing_evidence_or_projection_cuts(
) {
    use crate::trace::publication::ProjectionCatchUp;
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let text = "v".repeat(mapping::DERIVED_PAYLOAD_CHUNK_BYTES + 301);
    let mut raw=serde_json::to_vec(&serde_json::json!({"type":"response_item","payload":{"type":"message","id":"validation-body","role":"assistant","content":[{"type":"output_text","text":text}]}})).unwrap();
    raw.push(b'\n');
    let native = fixture.native(2, header.len() as u64, &raw).await;
    let (generation, _) = fixture.generation("validation").await;
    let worker = fixture.worker();
    worker.catch_up(&generation, 1, 2).await.unwrap();
    worker.validate_generation(&generation, 2).await.unwrap();
    let revision = claims(&generation)
        .await
        .into_iter()
        .find_map(|claim| match claim.fact {
            Fact::Declare {
                object: TraceObject::MessageRevision(value),
            } => Some(value),
            _ => None,
        })
        .unwrap();
    let manifest = fixture
        .evidence
        .payload_manifest(&revision.parts[0].payload)
        .await
        .unwrap()
        .unwrap();
    BlobStore::new(fixture.directory.path().join("blobs"))
        .remove(manifest.chunks[0].digest.digest())
        .await
        .unwrap();
    assert!(worker.validate_generation(&generation, 2).await.is_err());
    assert_eq!(
        generation.checkpoint().await.unwrap().snapshot.evidence_cut,
        2
    );
    assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), 2);
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&native.observation_ref, raw.len())
            .await
            .unwrap(),
        Some(raw)
    );
}

#[tokio::test]
#[ignore = "manual real-pipeline scaling measurement; run in its own process"]
async fn worker_projection_scaling_probe() {
    let records: usize = std::env::var("SYLPH_WORKER_BENCH_RECORDS")
        .unwrap_or_else(|_| "30".into())
        .parse()
        .unwrap();
    let fixture = Fixture::new().await;
    let admission = std::time::Instant::now();
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let mut offset = header.len() as u64;
    for index in 0..records {
        let mut raw=serde_json::to_vec(&serde_json::json!({"type":"response_item","payload":{"type":"message","id":format!("scale-{index}"),"role":"assistant","content":[{"type":"output_text","text":format!("measured message {index}")}]}})).unwrap();
        raw.push(b'\n');
        fixture.native(index as u64 + 2, offset, &raw).await;
        offset += raw.len() as u64;
    }
    let admission_ms = admission.elapsed().as_millis();
    let (generation, _) = fixture.generation("scaling").await;
    let started = std::time::Instant::now();
    fixture
        .worker()
        .catch_up(&generation, 1, records as u64 + 1)
        .await
        .unwrap();
    let projection_ms = started.elapsed().as_millis();
    let snapshot = generation.checkpoint().await.unwrap().snapshot;
    let mut count = 0;
    let mut cursor = None;
    loop {
        let page = generation
            .page(
                &snapshot,
                &QueryScope::archive(agent()),
                cursor.as_deref(),
                generation.limits().max_items,
            )
            .await
            .unwrap();
        count += page.objects.len();
        if !page.has_more {
            break;
        }
        cursor = page.next_cursor;
    }
    assert_eq!(count, records);
    eprintln!("worker scaling: records={records} admission_ms={admission_ms} projection_ms={projection_ms} revision={}",snapshot.view_revision);
}

#[tokio::test]
async fn structural_decode_limit_retains_raw_bytes_and_suspends_uncertain_source_continuation() {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let (generation, _) = fixture.generation("structural-limit").await;
    let worker = fixture.worker_with(WorkerLimits {
        max_json_nodes: 64,
        ..WorkerLimits::default()
    });
    worker.catch_up(&generation, 1, 1).await.unwrap();
    let calls = worker.mapping_threads.lock().unwrap().len();
    let content: Vec<_> = (0..100)
        .map(|_| serde_json::json!({"type":"output_text","text":"part"}))
        .collect();
    let mut raw=serde_json::to_vec(&serde_json::json!({"type":"response_item","payload":{"type":"message","id":"large-shape","role":"assistant","content":content}})).unwrap();
    raw.push(b'\n');
    let captured = fixture.native(2, header.len() as u64, &raw).await;
    let limited = worker.catch_up(&generation, 1, 2).await.unwrap();
    assert_eq!(limited.suspended_sources, 1);
    assert_eq!(limited.unmapped_observations, 1);
    assert!(limited
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::ResourceLimit));
    assert_eq!(worker.mapping_threads.lock().unwrap().len(), calls);
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&captured.observation_ref, raw.len())
            .await
            .unwrap(),
        Some(raw.clone())
    );
    let later=b"{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"after-gap\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"not safely associated\"}]}}\n";
    fixture
        .native(3, header.len() as u64 + raw.len() as u64, later)
        .await;
    let continued = fixture.worker().catch_up(&generation, 1, 3).await.unwrap();
    assert_eq!(continued.suspended_sources, 1);
    assert_eq!(continued.unmapped_observations, 1);
    assert!(revision_bytes(&fixture, &generation, &message("after-gap"))
        .await
        .is_empty());
}

#[tokio::test]
async fn malformed_record_never_reuses_a_pre_gap_activation_for_later_content() {
    let fixture = Fixture::new().await;
    let mut offset = 0;
    for (index, line) in CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .take(2)
        .enumerate()
    {
        fixture.native(index as u64 + 1, offset, line).await;
        offset += line.len() as u64;
    }
    let malformed = b"{broken JSON\n";
    let capture = fixture.native(3, offset, malformed).await;
    offset += malformed.len() as u64;
    let answer = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .nth(3)
        .unwrap();
    fixture.native(4, offset, answer).await;
    let (generation, _) = fixture.generation("malformed-gap").await;
    let progress = fixture.worker().catch_up(&generation, 1, 4).await.unwrap();
    assert_eq!(progress.suspended_sources, 1);
    assert_eq!(progress.unmapped_observations, 2);
    assert!(progress
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::DecodeFailure));
    assert!(revision_bytes(&fixture, &generation, &message("answer-1"))
        .await
        .is_empty());
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&capture.observation_ref, malformed.len())
            .await
            .unwrap(),
        Some(malformed.to_vec())
    );
}

#[test]
fn streaming_json_limits_bound_structure_and_identity_while_preserving_large_text() {
    let limits = WorkerLimits {
        max_json_depth: 4,
        max_identity_bytes: 8,
        ..WorkerLimits::default()
    };
    assert_eq!(
        budget::validate(b"[[[[[[0]]]]]]", &limits),
        Err(budget::Failure::ResourceLimit)
    );
    assert_eq!(
        budget::validate(b"{\"id\":\"identity-too-long\"}", &limits),
        Err(budget::Failure::ResourceLimit)
    );
    assert_eq!(
        budget::validate(b"{bad}", &limits),
        Err(budget::Failure::Malformed)
    );
    let text = serde_json::to_vec(&serde_json::json!({"text":"x".repeat(10000)})).unwrap();
    assert_eq!(budget::validate(&text, &limits), Ok(()));
}

#[tokio::test(flavor = "current_thread")]
async fn vendor_mapping_executes_outside_the_async_runtime_thread() {
    let runtime_thread = std::thread::current().id();
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let (generation, _) = fixture.generation("cpu-worker").await;
    let worker = fixture.worker();
    worker.catch_up(&generation, 1, 1).await.unwrap();
    let observed = worker.mapping_threads.lock().unwrap();
    assert!(!observed.is_empty());
    assert!(observed.iter().all(|thread| *thread != runtime_thread));
}

#[tokio::test]
async fn long_message_preview_is_an_exact_utf8_prefix_with_explicit_truncation() {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let text = format!("a{}", "\u{1f98b}".repeat(600));
    let mut raw=serde_json::to_vec(&serde_json::json!({"type":"response_item","payload":{"type":"message","id":"unicode-preview","role":"assistant","content":[{"type":"output_text","text":text}]}})).unwrap();
    raw.push(b'\n');
    fixture.native(2, header.len() as u64, &raw).await;
    let (generation, _) = fixture.generation("utf8-preview").await;
    fixture.worker().catch_up(&generation, 1, 2).await.unwrap();
    let revision = claims(&generation)
        .await
        .into_iter()
        .find_map(|claim| match claim.fact {
            Fact::Declare {
                object: TraceObject::MessageRevision(value),
            } => Some(value),
            _ => None,
        })
        .unwrap();
    let summary = generation
        .summary(
            &generation.checkpoint().await.unwrap().snapshot,
            &AnyObjectRef::MessageRevision(revision.revision_ref),
        )
        .await
        .unwrap();
    let expected = format!(
        "a{}",
        "\u{1f98b}".repeat((generation.limits().max_preview_bytes - 1) / 4)
    );
    assert_eq!(summary.preview, expected);
    assert!(summary.preview_truncated);
    assert_eq!(
        revision_bytes(&fixture, &generation, &message("unicode-preview")).await,
        vec![text.into_bytes()]
    );
}

#[tokio::test]
async fn observation_units_split_at_generation_bounds_without_changing_claims_or_source_order() {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let mut offset = header.len() as u64;
    for index in 0..24 {
        let mut raw=serde_json::to_vec(&serde_json::json!({"type":"response_item","payload":{"type":"message","id":format!("bounded-{index}"),"role":"assistant","content":[{"type":"output_text","text":format!("bounded {index}")}]}})).unwrap();
        raw.push(b'\n');
        fixture.native(index + 2, offset, &raw).await;
        offset += raw.len() as u64;
    }
    let (split, _) = fixture
        .generation_with_limits(
            "split",
            QueryLimits {
                max_batch_changes: 128,
                ..QueryLimits::default()
            },
        )
        .await;
    fixture.worker().catch_up(&split, 1, 25).await.unwrap();
    let checkpoint = split.checkpoint().await.unwrap();
    assert!(checkpoint.snapshot.view_revision > 1 && checkpoint.snapshot.view_revision < 25);
    let (cold, _) = fixture.generation("unsplit").await;
    fixture.worker().catch_up(&cold, 1, 25).await.unwrap();
    assert_eq!(claims(&split).await, claims(&cold).await);
    assert_eq!(
        revision_bytes(&fixture, &split, &message("bounded-23")).await,
        vec![b"bounded 23".to_vec()]
    );
    assert_eq!(
        split
            .page(
                &checkpoint.snapshot,
                &QueryScope::archive(agent()),
                None,
                32
            )
            .await
            .unwrap()
            .total_count,
        24
    );
}

#[tokio::test]
async fn retained_companion_inventory_resolves_metadata_owner_after_restart_without_filesystem_reads(
) {
    let fixture = Fixture::for_vendor("claude").await;
    let root = SessionId::decode("env7fk2:claude:root-session").unwrap();
    let child = AgentRef::Embedded {
        anchor_session: root,
        namespace: "claude.agent".into(),
        native_child_id: "worker-a".into(),
    };
    let source = SourceRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
        instance: "child-native".into(),
    };
    let proof = ObservationRef {
        issuer: source.issuer.clone(),
        producer_id: "delegate".into(),
        producer_epoch: "one".into(),
        producer_seq: 1,
    };
    let raw=b"{\"type\":\"assistant\",\"sessionId\":\"root-session\",\"agentId\":\"worker-a\",\"uuid\":\"message-a\",\"message\":{\"id\":\"model-a\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"child owned answer\"}]}}\n";
    fixture
        .admit(
            proof.clone(),
            SourcePosition {
                source: source.clone(),
                generation: 0,
                locator: SourceLocator::Jsonl {
                    byte_start: 0,
                    byte_end: raw.len() as u64,
                    record_ordinal: 0,
                },
            },
            raw,
            TransportKind::NativeJsonl,
        )
        .await;
    let (generation, _) = fixture.generation("companion").await;
    fixture.worker().catch_up(&generation, 1, 1).await.unwrap();
    let definition = SourceDefinition {
        source: source.clone(),
        generation: 0,
        locator: "/absent/vendor/transcript.jsonl".into(),
        owner: child.clone(),
        basis: SourceOwnerBasis::NativeIdentity { observation: proof },
    };
    let inventory_source = SourceRef {
        issuer: source.issuer.clone(),
        instance: "source-inventory".into(),
    };
    fixture
        .admit(
            ObservationRef {
                issuer: source.issuer.clone(),
                producer_id: "inventory".into(),
                producer_epoch: "one".into(),
                producer_seq: 1,
            },
            SourcePosition {
                source: inventory_source.clone(),
                generation: 0,
                locator: SourceLocator::Frame { ordinal: 1 },
            },
            &serde_json::to_vec(&definition).unwrap(),
            TransportKind::SourceInventory,
        )
        .await;
    let sidecar = SourceRef {
        issuer: source.issuer.clone(),
        instance: "metadata-sidecar".into(),
    };
    let companion = SourceDefinition {
        source: sidecar.clone(),
        generation: 0,
        locator: "/absent/vendor/transcript.meta.json".into(),
        owner: child.clone(),
        basis: SourceOwnerBasis::CompanionTranscript {
            source,
            generation: 0,
        },
    };
    fixture
        .admit(
            ObservationRef {
                issuer: sidecar.issuer.clone(),
                producer_id: "inventory".into(),
                producer_epoch: "one".into(),
                producer_seq: 2,
            },
            SourcePosition {
                source: inventory_source,
                generation: 0,
                locator: SourceLocator::Frame { ordinal: 2 },
            },
            &serde_json::to_vec(&companion).unwrap(),
            TransportKind::SourceInventory,
        )
        .await;
    let meta = b"{\"parentAgentId\":\"parent-worker\",\"name\":\"delegate-name\"}";
    fixture
        .admit(
            ObservationRef {
                issuer: sidecar.issuer.clone(),
                producer_id: "metadata".into(),
                producer_epoch: "one".into(),
                producer_seq: 1,
            },
            SourcePosition {
                source: sidecar,
                generation: 0,
                locator: SourceLocator::Snapshot {
                    revision: "snapshot-one".into(),
                },
            },
            meta,
            TransportKind::NativeSnapshot,
        )
        .await;
    let progress = fixture.worker().catch_up(&generation, 1, 4).await.unwrap();
    assert_eq!(progress.pending_mappings, 0);
    let parent = AgentRef::Embedded {
        anchor_session: child.anchor_session().clone(),
        namespace: "claude.agent".into(),
        native_child_id: "parent-worker".into(),
    };
    let claims = claims(&generation).await;
    assert!(claims.iter().any(|claim|matches!(&claim.fact,Fact::Relation{relation:Relation::Spawn{parent:actual_parent,child:actual_child,..}}if actual_parent==&parent&&actual_child==&child)));
    let object = generation
        .object(
            &generation.checkpoint().await.unwrap().snapshot,
            &AnyObjectRef::Agent(child),
        )
        .await
        .unwrap();
    let TraceObject::Agent(agent) = object.object else {
        panic!("embedded agent")
    };
    assert_eq!(agent.name.as_deref(), Some("delegate-name"));
}

mod delayed_source;
mod frozen_history;

mod streaming;

#[tokio::test]
async fn shared_archive_snapshots_keep_prior_facts_and_refresh_later_reduction_proofs() {
    let fixture=Fixture::new().await;fixture.native_fixture().await;
    let (generation,_)=fixture.generation("shared-archive").await;
    let worker=fixture.worker();worker.catch_up(&generation,1,2).await.unwrap();
    let held=worker.states.lock().await.get("shared-archive").unwrap().clone();
    let previous=archive_context(&mut *held.lock().await);
    assert!(previous.semantic.history_entries.is_empty());
    worker.catch_up(&generation,1,5).await.unwrap();
    let current=archive_context(&mut *held.lock().await);
    assert_eq!(current.semantic.history_entries.len(),2);
    assert!(previous.semantic.history_entries.is_empty(),"old mapper inputs cannot be mutated by later projection");
    for entry in current.semantic.history_entries.values() {
        let evidence=current.semantic.object_evidence.get(&AnyObjectRef::HistoryEntry(entry.entry_ref.clone())).unwrap();
        assert!(!evidence.is_empty());assert!(evidence.iter().all(|observation|current.observations.contains(observation)));
    }
    let mut fork=current.clone();fork.semantic_mut().history_entries.clear();
    assert_eq!(current.semantic.history_entries.len(),2,"a candidate context cannot mutate another mapping input");
}
