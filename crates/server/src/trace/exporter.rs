//! Transfer fixed operational occurrences into evidence without dispatching commands.
use std::sync::Arc;
use sylph_protocol::PayloadDeclaration;
use tokio::sync::Mutex;
#[cfg(test)]
type ExportPause = Arc<Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>>;

use super::{
    evidence::{EvidenceError, ReceiptDisposition},
    upload::{BeginCapture, CaptureUploads, UploadError},
};
use crate::db::control::{ControlStore, ExportDisposition, OutboxError, OutboxOccurrence};
#[cfg(test)]
use sylph_protocol::trace::{CommandTarget, IngestionScope};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    #[error(transparent)]
    Upload(#[from] UploadError),
    #[error("staged operational bytes do not match the fixed outbox capture")]
    PayloadMismatch,
    #[error("control payload limits exceed evidence admission limits")]
    IncompatibleLimits,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportProgress {
    pub transferred: usize,
    pub retained: usize,
    pub discarded_by_deletion: usize,
}

#[derive(Clone)]
pub struct OutboxExporter {
    control: ControlStore,
    uploads: CaptureUploads,
    gate: Arc<Mutex<()>>,
    #[cfg(test)]
    pause_after_begin: ExportPause,
}

impl OutboxExporter {
    pub fn new(control: ControlStore, uploads: CaptureUploads) -> Result<Self, ExportError> {
        let control_limits = control.limits();
        let evidence_limits = uploads.evidence().limits();
        if control_limits.payload_chunk_bytes > evidence_limits.max_chunk_bytes
            || control_limits.max_payload_chunks > evidence_limits.max_manifest_chunks
            || control_limits.max_capture_envelope_bytes > evidence_limits.max_batch_envelope_bytes
            || control_limits.max_manifest_bytes > evidence_limits.max_batch_envelope_bytes
        {
            return Err(ExportError::IncompatibleLimits);
        }
        // Acceptance bounds must imply upload declaration bounds. Otherwise a
        // durable accepted command could be permanently unexportable without any
        // configuration changing between acceptance and transfer.
        let chunks = control_limits
            .max_fact_bytes
            .div_ceil(control_limits.payload_chunk_bytes)
            .min(control_limits.max_payload_chunks);
        let declarations = if control_limits.max_fact_bytes > control_limits.payload_chunk_bytes {
            chunks.checked_add(1)
        } else {
            Some(1)
        }
        .ok_or(ExportError::IncompatibleLimits)?;
        let worst = worst_declaration_bytes(
            control_limits.max_fact_bytes,
            control_limits.payload_chunk_bytes,
            chunks,
        )?;
        if declarations > uploads.limits().max_declarations
            || worst > uploads.limits().max_metadata_bytes
        {
            return Err(ExportError::IncompatibleLimits);
        }
        Ok(Self {
            control,
            uploads,
            gate: Arc::new(Mutex::new(())),
            #[cfg(test)]
            pause_after_begin: Arc::new(Mutex::new(None)),
        })
    }

    /// A bounded pass advances only after a terminal durable evidence receipt.
    /// Admission and marking are deliberately separate commits: retrying the exact
    /// occurrence closes that crash window without repeating a vendor effect.
    pub async fn run_once(&self) -> Result<ExportProgress, ExportError> {
        // API flushes and the background pass share one bounded producer. Upload
        // tickets are independently idempotent across separately opened exporters.
        let _guard = self.gate.lock().await;
        let occurrences = self.control.pending().await?;
        let mut progress = ExportProgress::default();
        for occurrence in occurrences {
            let disposition = self.admit(&occurrence).await?;
            self.control
                .mark_exported(&occurrence.capture.observation_ref, disposition)
                .await?;
            progress.transferred += 1;
            match disposition {
                ExportDisposition::Retained { .. } => progress.retained += 1,
                ExportDisposition::DiscardedByDeletion => progress.discarded_by_deletion += 1,
            }
        }
        Ok(progress)
    }

    async fn admit(&self, occurrence: &OutboxOccurrence) -> Result<ExportDisposition, ExportError> {
        let mut declarations = std::collections::BTreeMap::new();
        if let Some(manifest) = &occurrence.manifest {
            declarations.insert(
                occurrence.capture.payload_ref.clone(),
                PayloadDeclaration::Manifest {
                    reference: occurrence.capture.payload_ref.clone(),
                    manifest: manifest.clone(),
                },
            );
            for chunk in &manifest.chunks {
                declarations.insert(
                    chunk.digest.clone(),
                    PayloadDeclaration::Bytes {
                        reference: chunk.digest.clone(),
                        length: chunk.length,
                    },
                );
            }
        } else {
            declarations.insert(
                occurrence.capture.payload_ref.clone(),
                PayloadDeclaration::Bytes {
                    reference: occurrence.capture.payload_ref.clone(),
                    length: occurrence.payload.len() as u64,
                },
            );
        }
        let batch = match self
            .uploads
            .begin(
                &occurrence.capture.observation_ref.issuer,
                occurrence.capture.clone(),
                declarations.into_values().collect(),
            )
            .await?
        {
            BeginCapture::Complete(batch) => batch,
            BeginCapture::Pending(ticket) => {
                #[cfg(test)]
                if let Some((entered, resume)) = self.pause_after_begin.lock().await.take() {
                    entered.notify_one();
                    resume.notified().await;
                }
                if let Some(manifest) = &occurrence.manifest {
                    for chunk in &manifest.chunks {
                        let start = usize::try_from(chunk.payload_offset)
                            .map_err(|_| ExportError::PayloadMismatch)?;
                        let length = usize::try_from(chunk.length)
                            .map_err(|_| ExportError::PayloadMismatch)?;
                        let end = start
                            .checked_add(length)
                            .ok_or(ExportError::PayloadMismatch)?;
                        let bytes = occurrence
                            .payload
                            .get(start..end)
                            .ok_or(ExportError::PayloadMismatch)?;
                        if chunk.chunk_offset != 0 {
                            return Err(ExportError::PayloadMismatch);
                        }
                        self.uploads
                            .put_chunk(
                                &occurrence.capture.observation_ref.issuer,
                                &ticket,
                                &chunk.digest,
                                bytes,
                            )
                            .await?;
                    }
                } else {
                    self.uploads
                        .put_chunk(
                            &occurrence.capture.observation_ref.issuer,
                            &ticket,
                            &occurrence.capture.payload_ref,
                            &occurrence.payload,
                        )
                        .await?;
                }
                self.uploads
                    .finish(&occurrence.capture.observation_ref.issuer, &ticket)
                    .await?
            }
        };
        let receipt = batch.receipts.first().ok_or(ExportError::PayloadMismatch)?;
        Ok(match &receipt.disposition {
            ReceiptDisposition::Retained { admission_seq } => ExportDisposition::Retained {
                admission_cut: *admission_seq,
            },
            ReceiptDisposition::DiscardedByDeletion { .. } => {
                ExportDisposition::DiscardedByDeletion
            }
        })
    }
}

fn worst_declaration_bytes(
    max_bytes: usize,
    chunk_bytes: usize,
    max_chunks: usize,
) -> Result<usize, ExportError> {
    use sylph_protocol::trace::{
        PayloadChunk, PayloadCompleteness, PayloadEncoding, PayloadManifest, PayloadRef,
    };
    let digest = PayloadRef::bytes("f".repeat(64)).map_err(|_| ExportError::IncompatibleLimits)?;
    if max_bytes <= chunk_bytes {
        return Ok(serde_json::to_vec(&vec![PayloadDeclaration::Bytes {
            reference: digest,
            length: max_bytes as u64,
        }])
        .map_err(|_| ExportError::IncompatibleLimits)?
        .len());
    }
    let parts = (0..max_chunks)
        .map(|i| PayloadChunk {
            digest: digest.clone(),
            chunk_offset: 0,
            length: chunk_bytes as u64,
            payload_offset: i.saturating_mul(chunk_bytes) as u64,
        })
        .collect();
    let manifest = PayloadManifest {
        chunks: parts,
        media_type: "application/json".into(),
        encoding: PayloadEncoding::Utf8,
        decoded_length: Some(max_chunks.saturating_mul(chunk_bytes) as u64),
        completeness: PayloadCompleteness::Complete,
    };
    let mut declarations = vec![PayloadDeclaration::Manifest {
        reference: PayloadRef::manifest("f".repeat(64))
            .map_err(|_| ExportError::IncompatibleLimits)?,
        manifest,
    }];
    declarations.extend((0..max_chunks).map(|_| PayloadDeclaration::Bytes {
        reference: digest.clone(),
        length: chunk_bytes as u64,
    }));
    Ok(serde_json::to_vec(&declarations)
        .map_err(|_| ExportError::IncompatibleLimits)?
        .len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::control::{CommandState, OutboxLimits};
    use crate::{
        trace::cas::BlobStore,
        trace::{
            evidence::{
                catalogue::{CatalogueLimits, EvidenceCatalogue},
                AdmissionLimits, EvidenceStore,
            },
            upload::UploadLimits,
        },
    };
    use sylph_protocol::{
        trace::{
            AgentRef, AuthorizationReceipt, CommandAcceptance, CommandKind, IngestionEpoch, Issuer,
            OperationalFact, RetryPolicy, SubmittedRevision,
        },
        SessionId,
    };

    struct Fixture {
        _dir: tempfile::TempDir,
        control: ControlStore,
        evidence: EvidenceStore,
        agent: AgentRef,
        epoch: IngestionEpoch,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:root").unwrap());
            let evidence = EvidenceStore::open(
                dir.path().join("evidence.db"),
                BlobStore::new(dir.path().join("blobs")),
                AdmissionLimits::default(),
            )
            .await
            .unwrap();
            let epoch = evidence
                .authorize_scope(
                    &IngestionScope::Agent(agent.clone()),
                    std::slice::from_ref(&agent),
                )
                .await
                .unwrap();
            let control = ControlStore::open(
                &dir.path().join("control.db"),
                Issuer::Server("server-1".into()),
                OutboxLimits::default(),
            )
            .await
            .unwrap();
            Self {
                _dir: dir,
                control,
                evidence,
                agent,
                epoch,
            }
        }

        async fn accept(&self, bytes: Vec<u8>) {
            self.control
                .accept(CommandAcceptance {
                    command_id: "command-1".into(),
                    target: CommandTarget::Agent {
                        agent: self.agent.clone(),
                    },
                    kind: CommandKind::Send,
                    input: SubmittedRevision {
                        revision: "input-1".into(),
                        media_type: "application/octet-stream".into(),
                        bytes,
                    },
                    authorization: AuthorizationReceipt {
                        subject: "user-1".into(),
                        decision_ref: "authorized-1".into(),
                        lease_ref: None,
                        evaluated_at_ms: 100,
                    },
                    ingestion_authorization_epoch: self.epoch.clone(),
                    retry_policy: RetryPolicy::AtMostOnce,
                    idempotency_key: None,
                    accepted_at_ms: 100,
                })
                .await
                .unwrap();
        }

        fn exporter(&self) -> OutboxExporter {
            let catalogue =
                EvidenceCatalogue::new(self.evidence.clone(), CatalogueLimits::default()).unwrap();
            OutboxExporter::new(
                self.control.clone(),
                CaptureUploads::new(catalogue, UploadLimits::default()).unwrap(),
            )
            .unwrap()
        }
    }

    #[tokio::test]
    async fn independently_opened_exporters_can_finish_the_same_inflight_upload() {
        let fixture = Fixture::new().await;
        fixture
            .accept(b"one exact operational occurrence".to_vec())
            .await;
        let first = fixture.exporter();
        let second = fixture.exporter();
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *first.pause_after_begin.lock().await = Some((entered.clone(), resume.clone()));
        let task = tokio::spawn(async move { first.run_once().await });
        entered.notified().await;
        assert_eq!(second.run_once().await.unwrap().retained, 1);
        resume.notify_one();
        assert_eq!(task.await.unwrap().unwrap().retained, 1);
        assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), 1);
        assert_eq!(
            fixture.control.progress(200).await.unwrap().pending_count,
            0
        );
    }

    #[tokio::test]
    async fn retry_after_evidence_commit_exports_once_without_dispatch() {
        let fixture = Fixture::new().await;
        fixture.accept(b"same text\r\n\0".to_vec()).await;
        let occurrence = fixture.control.pending().await.unwrap().remove(0);
        let exporter = fixture.exporter();
        // This is the crash boundary: evidence committed, control not yet marked.
        exporter.admit(&occurrence).await.unwrap();
        assert_eq!(
            fixture.control.progress(200).await.unwrap().pending_count,
            1
        );
        let cut = fixture.evidence.evidence_cut().await.unwrap();
        let result = exporter.run_once().await.unwrap();
        assert_eq!(result.transferred, 1);
        assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), cut);
        assert_eq!(
            fixture
                .evidence
                .capture_payload(
                    &occurrence.capture.observation_ref,
                    occurrence.payload.len()
                )
                .await
                .unwrap(),
            Some(occurrence.payload)
        );
        let command = fixture.control.command("command-1").await.unwrap().unwrap();
        assert_eq!(command.state, CommandState::Accepted);
        assert_eq!(command.dispatch_attempts, 0);
        assert_eq!(exporter.run_once().await.unwrap().transferred, 0);
    }

    #[tokio::test]
    async fn deleting_before_export_discards_the_fixed_epoch_without_uploading_bytes() {
        let fixture = Fixture::new().await;
        fixture.accept(b"deleted input".to_vec()).await;
        let occurrence = fixture.control.pending().await.unwrap().remove(0);
        fixture
            .evidence
            .delete_scope(&IngestionScope::Agent(fixture.agent.clone()))
            .await
            .unwrap();
        let fresh_epoch = fixture
            .evidence
            .readopt_scope(
                &IngestionScope::Agent(fixture.agent.clone()),
                std::slice::from_ref(&fixture.agent),
            )
            .await
            .unwrap();
        assert_ne!(
            fresh_epoch,
            occurrence.capture.ingestion_authorization_epoch
        );
        let result = fixture.exporter().run_once().await.unwrap();
        assert_eq!(result.discarded_by_deletion, 1);
        assert_eq!(result.retained, 0);
        assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), 0);
        assert!(fixture
            .evidence
            .payload_bytes(&occurrence.capture.payload_ref, occurrence.payload.len())
            .await
            .is_err());
        assert_eq!(
            fixture.control.progress(200).await.unwrap().pending_count,
            0
        );
        assert!(matches!(
            fixture
                .evidence
                .receipt(&occurrence.capture.observation_ref)
                .await
                .unwrap()
                .unwrap()
                .disposition,
            ReceiptDisposition::DiscardedByDeletion { .. }
        ));
    }

    #[tokio::test]
    async fn submitted_input_larger_than_a_transfer_chunk_retains_every_byte() {
        let fixture = Fixture::new().await;
        let chunk_bytes = fixture.evidence.limits().max_chunk_bytes;
        let input: Vec<u8> = (0..chunk_bytes + 257).map(|i| (i % 251) as u8).collect();
        fixture.accept(input.clone()).await;
        let occurrence = fixture.control.pending().await.unwrap().remove(0);
        assert!(occurrence.manifest.as_ref().unwrap().chunks.len() > 1);
        let result = fixture.exporter().run_once().await.unwrap();
        assert_eq!(result.retained, 1);
        let retained = fixture
            .evidence
            .capture_payload(
                &occurrence.capture.observation_ref,
                occurrence.payload.len(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained, occurrence.payload);
        let fact: OperationalFact = serde_json::from_slice(&retained).unwrap();
        let OperationalFact::Accepted { command } = fact else {
            panic!("accepted fact expected")
        };
        assert_eq!(command.input.bytes, input);
    }

    #[tokio::test]
    async fn incompatible_capture_limits_are_refused_before_an_exporter_can_accept_work() {
        let fixture = Fixture::new().await;
        let evidence = EvidenceStore::open(
            fixture._dir.path().join("smaller-evidence.db"),
            BlobStore::new(fixture._dir.path().join("other-blobs")),
            AdmissionLimits {
                max_chunk_bytes: 1024,
                ..AdmissionLimits::default()
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            OutboxExporter::new(
                fixture.control.clone(),
                CaptureUploads::new(
                    EvidenceCatalogue::new(evidence, CatalogueLimits::default()).unwrap(),
                    UploadLimits::default()
                )
                .unwrap()
            ),
            Err(ExportError::IncompatibleLimits)
        ));
    }

    #[tokio::test]
    async fn accepted_fact_limits_must_fit_upload_declaration_count_and_metadata_bytes() {
        let fixture = Fixture::new().await;
        for limits in [
            UploadLimits {
                max_declarations: 1,
                ..Default::default()
            },
            UploadLimits {
                max_metadata_bytes: 1,
                ..Default::default()
            },
        ] {
            let uploads = CaptureUploads::new(
                EvidenceCatalogue::new(fixture.evidence.clone(), CatalogueLimits::default())
                    .unwrap(),
                limits,
            )
            .unwrap();
            assert!(matches!(
                OutboxExporter::new(fixture.control.clone(), uploads),
                Err(ExportError::IncompatibleLimits)
            ));
        }
        assert!(fixture.exporter().run_once().await.is_ok());
    }
}
