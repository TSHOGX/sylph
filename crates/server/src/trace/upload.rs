//! Capture-owned byte transfer before durable evidence admission.
//!
//! A CAS address identifies bytes; possession of it does not prove that a source
//! captured them. Each new observation owns transfer receipts or a proven reread
//! of its same source occurrence in the same ingestion authorization epoch.

use super::evidence::{
    catalogue::{CatalogueError, EvidenceCatalogue},
    AdmissionBatch, CapturePreflight, EvidenceError, EvidenceStore,
};
use std::collections::{BTreeMap, BTreeSet};
use sylph_protocol::trace::{Capture, Issuer, PayloadKind, PayloadRef};
use sylph_protocol::{CaptureUploadTicket, PayloadDeclaration};

#[derive(Debug, Clone)]
pub struct UploadLimits {
    pub max_declarations: usize,
    pub max_metadata_bytes: usize,
    pub max_open_uploads: usize,
}
impl Default for UploadLimits {
    fn default() -> Self {
        Self {
            max_declarations: 8192,
            max_metadata_bytes: 1024 * 1024,
            max_open_uploads: 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("upload database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("upload encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error(transparent)]
    Catalogue(#[from] CatalogueError),
    #[error("invalid capture upload: {0}")]
    Invalid(String),
    #[error("capture upload is not owned by this authenticated issuer")]
    Authority,
    #[error("capture still needs its own byte transfer receipts")]
    Incomplete,
    #[error("upload bytes do not match their declared digest or length")]
    BytesMismatch,
    #[error("capture upload identity was reused with another declaration")]
    Collision,
    #[error("capture upload is cancelled or unavailable")]
    Unavailable,
    #[error("capture upload exceeds {0}")]
    Limit(&'static str),
}
pub type Result<T> = std::result::Result<T, UploadError>;

#[derive(Debug, Clone)]
pub enum BeginCapture {
    Complete(AdmissionBatch),
    Pending(CaptureUploadTicket),
}

pub(crate) struct AdmissionGuard {
    pub(crate) upload_id: String,
}

#[derive(Clone)]
pub struct CaptureUploads {
    catalogue: EvidenceCatalogue,
    limits: UploadLimits,
}
impl CaptureUploads {
    pub fn new(catalogue: EvidenceCatalogue, limits: UploadLimits) -> Result<Self> {
        if limits.max_declarations == 0
            || limits.max_metadata_bytes == 0
            || limits.max_open_uploads == 0
        {
            return Err(UploadError::Invalid(
                "upload limits must be positive".into(),
            ));
        }
        Ok(Self { catalogue, limits })
    }
    pub fn evidence(&self) -> &EvidenceStore {
        self.catalogue.evidence()
    }
    pub fn catalogue(&self) -> &EvidenceCatalogue {
        &self.catalogue
    }
    pub fn limits(&self) -> &UploadLimits {
        &self.limits
    }

    pub async fn begin(
        &self,
        issuer: &Issuer,
        capture: Capture,
        declarations: Vec<PayloadDeclaration>,
    ) -> Result<BeginCapture> {
        match self
            .evidence()
            .admission_preflight(issuer, &capture)
            .await?
        {
            CapturePreflight::Recorded(_) | CapturePreflight::DiscardedByDeletion { .. } => {
                return Ok(BeginCapture::Complete(
                    self.evidence().admit_batch(issuer, &[capture]).await?,
                ))
            }
            CapturePreflight::RequiresPayload => {}
        }
        let declarations = self.attachment_declarations(&capture, declarations).await?;
        let declarations = self.validate_declarations(&capture, declarations)?;
        let ticket = self
            .catalogue
            .begin_capture_upload(
                issuer,
                &capture,
                &declarations,
                self.limits.max_open_uploads,
            )
            .await?;
        Ok(BeginCapture::Pending(ticket))
    }

    pub async fn put_chunk(
        &self,
        issuer: &Issuer,
        ticket: &CaptureUploadTicket,
        reference: &PayloadRef,
        bytes: &[u8],
    ) -> Result<()> {
        self.put_chunk_inner(issuer, ticket, reference, bytes, false)
            .await
    }

    async fn put_chunk_inner(
        &self,
        issuer: &Issuer,
        ticket: &CaptureUploadTicket,
        reference: &PayloadRef,
        bytes: &[u8],
        crash_after_stage: bool,
    ) -> Result<()> {
        if bytes.len() > self.evidence().limits().max_chunk_bytes {
            return Err(UploadError::Limit("chunk bytes"));
        }
        let (expected, admitted) = self
            .catalogue
            .capture_chunk_status(issuer, ticket, reference)
            .await?;
        if expected != bytes.len() as u64
            || reference.kind() != PayloadKind::Bytes
            || blake3::hash(bytes).to_hex().as_str() != reference.digest()
        {
            return Err(UploadError::BytesMismatch);
        }
        // A concurrent finisher or lost response may leave this ticket already
        // admitted. Verify the exact declared bytes above, then reuse that durable
        // transfer proof without staging the payload again.
        if admitted {
            return Ok(());
        }
        if self.evidence().stage_payload(bytes).await? != *reference {
            return Err(UploadError::BytesMismatch);
        }
        #[cfg(test)]
        if crash_after_stage {
            #[cfg(unix)]
            if std::env::var_os("SYLPH_CAPTURE_UPLOAD_CRASH_PROCESS").is_some() {
                unsafe extern "C" {
                    fn kill(pid: i32, signal: i32) -> i32;
                }
                unsafe { kill(std::process::id() as i32, 9) };
                unreachable!("capture upload probe must die");
            }
            return Err(UploadError::Unavailable);
        }
        let _ = crash_after_stage;
        self.catalogue
            .record_capture_chunk(issuer, ticket, reference)
            .await?;
        Ok(())
    }

    pub async fn finish(
        &self,
        issuer: &Issuer,
        ticket: &CaptureUploadTicket,
    ) -> Result<AdmissionBatch> {
        let (capture, declarations) = self.catalogue.capture_upload(issuer, ticket).await?;
        if matches!(
            self.evidence()
                .admission_preflight(issuer, &capture)
                .await?,
            CapturePreflight::RequiresPayload
        ) {
            self.catalogue.capture_is_complete(issuer, ticket).await?;
            for declaration in declarations {
                if let PayloadDeclaration::Manifest {
                    reference,
                    manifest,
                } = declaration
                {
                    if self.evidence().stage_manifest(&manifest).await? != reference {
                        return Err(UploadError::BytesMismatch);
                    }
                }
            }
        }
        Ok(self
            .evidence()
            .admit_owned(
                issuer,
                &capture,
                &AdmissionGuard {
                    upload_id: ticket.id.clone(),
                },
            )
            .await?)
    }

    pub async fn cancel(&self, issuer: &Issuer, ticket: &CaptureUploadTicket) -> Result<()> {
        self.catalogue.cancel_capture_upload(issuer, ticket).await
    }

    /// Startup only, after the prior server process is gone. Ordinary upload
    /// retry cannot remove the durable guards of another in-flight filesystem write.
    pub async fn recover_interrupted_writes(&self) -> Result<()> {
        self.catalogue.recover_blob_writes().await?;
        Ok(())
    }

    /// Attachment observations describe an already captured sidecar occurrence.
    /// Its source proof survives vendor removal and queue acknowledgment; requiring
    /// its old bytes to be uploaded a second time would strand the producer. This
    /// read is advisory: the upload and admission transactions recheck the proof.
    async fn attachment_declarations(
        &self,
        capture: &Capture,
        mut declarations: Vec<PayloadDeclaration>,
    ) -> Result<Vec<PayloadDeclaration>> {
        let mut metadata_bytes = serde_json::to_vec(&declarations)?.len();
        if declarations.len() > self.limits.max_declarations
            || metadata_bytes > self.limits.max_metadata_bytes
        {
            return Err(UploadError::Limit("payload declarations"));
        }
        let mut declared: BTreeSet<_> = declarations
            .iter()
            .map(|value| value.reference().clone())
            .collect();
        if declared.len() != declarations.len() {
            return Err(UploadError::Invalid("duplicate payload declaration".into()));
        }
        for attachment in &capture.attachments {
            let Some(reference) = &attachment.payload_ref else {
                continue;
            };
            if declared.contains(reference) {
                continue;
            }
            if !self
                .catalogue
                .retained_attachment(capture, attachment)
                .await?
            {
                return Err(UploadError::Incomplete);
            }
            let mut chunks = BTreeSet::new();
            if let Some(manifest) = self.evidence().payload_manifest(reference).await? {
                chunks.extend(manifest.chunks.iter().map(|chunk| chunk.digest.clone()));
                self.add_attachment_declaration(
                    &mut declarations,
                    &mut declared,
                    &mut metadata_bytes,
                    PayloadDeclaration::Manifest {
                        reference: reference.clone(),
                        manifest,
                    },
                )?;
            } else {
                chunks.insert(reference.clone());
            }
            for chunk in chunks {
                if declared.contains(&chunk) {
                    continue;
                }
                let length = self
                    .evidence()
                    .payload_length(&chunk)
                    .await?
                    .ok_or(UploadError::Incomplete)?;
                self.add_attachment_declaration(
                    &mut declarations,
                    &mut declared,
                    &mut metadata_bytes,
                    PayloadDeclaration::Bytes {
                        reference: chunk,
                        length,
                    },
                )?;
            }
        }
        Ok(declarations)
    }

    fn add_attachment_declaration(
        &self,
        declarations: &mut Vec<PayloadDeclaration>,
        declared: &mut BTreeSet<PayloadRef>,
        metadata_bytes: &mut usize,
        value: PayloadDeclaration,
    ) -> Result<()> {
        *metadata_bytes = metadata_bytes.saturating_add(serde_json::to_vec(&value)?.len() + 1);
        if declarations.len() == self.limits.max_declarations
            || *metadata_bytes > self.limits.max_metadata_bytes
        {
            return Err(UploadError::Limit("retained attachment declarations"));
        }
        declared.insert(value.reference().clone());
        declarations.push(value);
        Ok(())
    }

    fn validate_declarations(
        &self,
        capture: &Capture,
        declarations: Vec<PayloadDeclaration>,
    ) -> Result<Vec<PayloadDeclaration>> {
        if declarations.len() > self.limits.max_declarations {
            return Err(UploadError::Limit("payload declarations"));
        }
        if serde_json::to_vec(&declarations)
            .map_err(|e| UploadError::Invalid(e.to_string()))?
            .len()
            > self.limits.max_metadata_bytes
        {
            return Err(UploadError::Limit("payload declaration bytes"));
        }
        let mut by_ref = BTreeMap::new();
        for declaration in declarations {
            if by_ref
                .insert(declaration.reference().clone(), declaration)
                .is_some()
            {
                return Err(UploadError::Invalid("duplicate payload declaration".into()));
            }
        }
        let mut needed: BTreeSet<_> = std::iter::once(capture.payload_ref.clone())
            .chain(
                capture
                    .attachments
                    .iter()
                    .filter_map(|a| a.payload_ref.clone()),
            )
            .collect();
        let roots = needed.clone();
        for reference in roots {
            let declaration = by_ref.get(&reference).ok_or(UploadError::Incomplete)?;
            if let PayloadDeclaration::Manifest {
                reference,
                manifest,
            } = declaration
            {
                if reference.kind() != PayloadKind::Manifest
                    || super::evidence::manifest_ref(manifest)? != *reference
                    || manifest.chunks.len() > self.evidence().limits().max_manifest_chunks
                {
                    return Err(UploadError::Invalid("invalid manifest declaration".into()));
                }
                for chunk in &manifest.chunks {
                    needed.insert(chunk.digest.clone());
                }
            }
        }
        if needed.len() != by_ref.len() || needed.iter().any(|key| !by_ref.contains_key(key)) {
            return Err(UploadError::Invalid(
                "declarations must match the capture's complete payload dependency set".into(),
            ));
        }
        for declaration in by_ref.values() {
            match declaration {
                PayloadDeclaration::Bytes { reference, length } => {
                    if reference.kind() != PayloadKind::Bytes
                        || *length > self.evidence().limits().max_chunk_bytes as u64
                    {
                        return Err(UploadError::Limit("declared chunk bytes"));
                    }
                }
                PayloadDeclaration::Manifest { manifest, .. } => {
                    for chunk in &manifest.chunks {
                        let Some(PayloadDeclaration::Bytes { length, .. }) =
                            by_ref.get(&chunk.digest)
                        else {
                            return Err(UploadError::Invalid(
                                "manifest chunks must declare bytes".into(),
                            ));
                        };
                        if chunk
                            .chunk_offset
                            .checked_add(chunk.length)
                            .is_none_or(|end| end > *length)
                        {
                            return Err(UploadError::Invalid(
                                "manifest range exceeds declared chunk".into(),
                            ));
                        }
                    }
                }
            }
        }
        Ok(by_ref.into_values().collect())
    }
}

#[cfg(test)]
mod tests;
