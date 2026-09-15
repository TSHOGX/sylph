//! Transfer fixed operational occurrences into evidence without dispatching commands.

use super::{
    evidence::{EvidenceError, EvidenceStore, ReceiptDisposition},
    outbox::{ControlStore, ExportDisposition, OutboxError, OutboxOccurrence},
};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
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
    evidence: EvidenceStore,
}

impl OutboxExporter {
    pub fn new(control: ControlStore, evidence: EvidenceStore) -> Result<Self, ExportError> {
        let control_limits = control.limits();
        let evidence_limits = evidence.limits();
        if control_limits.payload_chunk_bytes > evidence_limits.max_chunk_bytes
            || control_limits.max_payload_chunks > evidence_limits.max_manifest_chunks
            || control_limits.max_capture_envelope_bytes > evidence_limits.max_batch_envelope_bytes
            || control_limits.max_manifest_bytes > evidence_limits.max_batch_envelope_bytes
        {
            return Err(ExportError::IncompatibleLimits);
        }
        Ok(Self { control, evidence })
    }

    /// A bounded pass advances only after a terminal durable evidence receipt.
    /// Admission and marking are deliberately separate commits: retrying the exact
    /// occurrence closes that crash window without repeating a vendor effect.
    pub async fn run_once(&self) -> Result<ExportProgress, ExportError> {
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
        // The metadata check is advisory. Admission rechecks the epoch/receipt in
        // its writer transaction, including deletion racing this payload staging.
        if self.evidence.needs_payloads(&occurrence.capture).await? {
            let payload_ref = if let Some(manifest) = &occurrence.manifest {
                for chunk in &manifest.chunks {
                    let start = usize::try_from(chunk.payload_offset)
                        .map_err(|_| ExportError::PayloadMismatch)?;
                    let length =
                        usize::try_from(chunk.length).map_err(|_| ExportError::PayloadMismatch)?;
                    let end = start
                        .checked_add(length)
                        .ok_or(ExportError::PayloadMismatch)?;
                    let bytes = occurrence
                        .payload
                        .get(start..end)
                        .ok_or(ExportError::PayloadMismatch)?;
                    if chunk.chunk_offset != 0
                        || self.evidence.stage_payload(bytes).await? != chunk.digest
                    {
                        return Err(ExportError::PayloadMismatch);
                    }
                }
                self.evidence.stage_manifest(manifest).await?
            } else {
                self.evidence.stage_payload(&occurrence.payload).await?
            };
            if payload_ref != occurrence.capture.payload_ref {
                return Err(ExportError::PayloadMismatch);
            }
        }
        let batch = self
            .evidence
            .admit_batch(
                &occurrence.capture.observation_ref.issuer,
                std::slice::from_ref(&occurrence.capture),
            )
            .await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        storage::cas::BlobStore,
        trace::{
            evidence::AdmissionLimits,
            outbox::{
                AuthorizationReceipt, CommandAcceptance, CommandKind, CommandState, OutboxLimits,
                RetryPolicy, SubmittedRevision,
            },
        },
    };
    use sylph_protocol::{
        trace::{AgentRef, IngestionEpoch, Issuer},
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
                .authorize_scope(&agent, std::slice::from_ref(&agent))
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
                    target: self.agent.clone(),
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
            OutboxExporter::new(self.control.clone(), self.evidence.clone()).unwrap()
        }
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
        fixture.evidence.delete_scope(&fixture.agent).await.unwrap();
        let fresh_epoch = fixture
            .evidence
            .readopt_scope(&fixture.agent, std::slice::from_ref(&fixture.agent))
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
        let fact: crate::trace::outbox::OperationalFact =
            serde_json::from_slice(&retained).unwrap();
        let crate::trace::outbox::OperationalFact::Accepted { command } = fact else {
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
            OutboxExporter::new(fixture.control.clone(), evidence),
            Err(ExportError::IncompatibleLimits)
        ));
    }
}
