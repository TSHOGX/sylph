//! Sidecar acquisition follows immutable parent observations without rewriting them.
use super::*;
use crate::spool::{attachments::AttachmentWatch, capture::QueuedCaptureMetadata};
use trace::{AttachmentAvailability, CaptureAttachment, TransportKind};

impl EnvAgent {
    pub(super) async fn discover_attachments(
        &self,
        pending: &QueuedCaptureMetadata,
    ) -> Result<bool> {
        let capture = &pending.capture;
        if !matches!(
            capture.transport_kind,
            TransportKind::LiveStdout | TransportKind::NativeJsonl | TransportKind::NativeSnapshot
        ) || self
            .spool
            .attachments_scanned(&capture.observation_ref)
            .await?
        {
            return Ok(false);
        }
        let mut raw = Vec::new();
        for ordinal in 0..pending.chunk_count {
            let (_, bytes) = self
                .registry
                .queued_chunk(&capture.observation_ref, ordinal)
                .await?
                .context("sidecar discovery requires retained parent bytes")?;
            raw.extend_from_slice(&bytes);
        }
        let parent_path=self.registry.source_locator(&capture.source_position.source).await?;
        let watches: Vec<_> = sylph_adapters::native::inventory::sidecar_references(&raw, capture.transport_kind)
            .into_iter()
            .map(|sidecar| AttachmentWatch {
                parent: capture.observation_ref.clone(),
                locator: if sidecar.relation.starts_with("codex.raw.") && Path::new(&sidecar.locator).is_relative() {
                    parent_path.as_ref().and_then(|path|path.parent()).map(|base|base.join(&sidecar.locator).to_string_lossy().into_owned()).unwrap_or(sidecar.locator)
                } else {sidecar.locator},
                parent_slot: sidecar.parent_slot,
                relation: sidecar.relation,
                authorization: LocalAuthorization {
                    epoch: capture.ingestion_authorization_epoch.clone(),
                    subjects: capture.declared_subject_refs.clone(),
                    captured_at: capture.captured_at.clone(),
                    vendor_build: capture.vendor_build.clone(),
                },
            })
            .collect();
        let has_watches = !watches.is_empty();
        self.spool
            .watch_attachments(&capture.observation_ref, watches)
            .await?;
        Ok(has_watches)
    }

    pub(super) async fn refresh_attachments(&self) -> Result<()> {
        for (watch, previous) in self.spool.attachment_watches(32).await? {
            let mut authorization = watch.authorization.clone();
            authorization.captured_at.value = sylph_adapters::now_ms();
            let mut attachment = CaptureAttachment {
                locator: watch.locator.clone(),
                source_position: None,
                observed_version: None,
                payload_ref: None,
                availability: AttachmentAvailability::Pending,
                relation: watch.relation.clone(),
            };
            let observed: Result<()> = async {
                anyhow::ensure!(
                    Path::new(&watch.locator).is_absolute(),
                    "relative sidecar has no evidenced acquisition base"
                );
                let source = self
                    .registry
                    .register_jsonl("vendor.output-sidecar", &watch.locator)
                    .await?;
                let pass = self
                    .registry
                    .capture_snapshot_as(
                        &source,
                        authorization.clone(),
                        TransportKind::PayloadBytes,
                    )
                    .await?;
                if pass.source_missing {
                    attachment.availability = AttachmentAvailability::Missing;
                    return Ok(());
                }
                let captured = self
                    .registry
                    .snapshot_capture(&source.reference, pass.generation, &authorization.epoch)
                    .await?
                    .context("sidecar snapshot is not retained")?;
                attachment.source_position = Some(captured.source_position);
                attachment.observed_version = Some(pass.generation.to_string());
                attachment.payload_ref = Some(captured.payload_ref);
                attachment.availability = AttachmentAvailability::Captured;
                Ok(())
            }
            .await;
            if let Err(error) = observed {
                attachment.availability = if error
                    .downcast_ref::<crate::spool::capture::CaptureSpoolError>()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            crate::spool::capture::CaptureSpoolError::ChangedDuringRead
                        )
                    }) {
                    AttachmentAvailability::Pending
                } else {
                    AttachmentAvailability::Unreadable {
                        reason: format!("{error:#}"),
                    }
                };
            }
            let state = serde_json::to_string(&attachment)?;
            if previous.as_deref() != Some(&state) {
                let event = SourceAttachmentEvent {
                    parent_observation: watch.parent.clone(),
                    parent_slot: watch.parent_slot.clone(),
                    attachment: attachment.clone(),
                };
                self.registry
                    .capture_frame_with_attachments(
                        "source-attachments",
                        TransportKind::SourceAttachment,
                        serde_json::to_vec(&event)?,
                        vec![attachment],
                        authorization,
                    )
                    .await?;
            }
            self.spool
                .record_attachment_check(&watch, &state, sylph_adapters::now_ms())
                .await?;
        }
        Ok(())
    }
}
