//! Lossless acquisition envelopes, immutable payload manifests and explicit source clocks.
//!
//! `docs/unified-trace-storage.md` § Capture contract owns admission. These types perform
//! structural validation only; the archive verifies bytes and durability before receipt.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};

use super::identity::{
    nonempty, AgentRef, Issuer, ObservationRef, SourcePosition, TraceIdentityError,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error(transparent)]
    Identity(#[from] TraceIdentityError),
    #[error(
        "a payload address must name bytes or manifest followed by its lowercase hexadecimal BLAKE3 digest"
    )]
    PayloadAddress,
    #[error("invalid payload manifest: {0}")]
    Manifest(&'static str),
    #[error("invalid capture attachment: {0}")]
    Attachment(&'static str),
    #[error("capture producer and source have different issuers")]
    IssuerMismatch,
    #[error("an environment capture cannot declare a subject in another environment")]
    SubjectEnvironment,
    #[error("invalid clock evidence: {0}")]
    Clock(&'static str),
}

/// Logical payload kind is distinct from the digest of its physical CAS object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    Bytes,
    Manifest,
}

/// Typed logical addresses prevent raw bytes from being interpreted as a manifest.
///
/// Any manifest encoding can itself occur as arbitrary retained source bytes. A magic
/// prefix within the bytes cannot separate those meanings; the reference kind must.
/// Both logical references may safely share the same immutable physical CAS object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PayloadRef(String);

impl PayloadRef {
    pub fn bytes(digest: impl AsRef<str>) -> Result<Self, CaptureError> {
        Self::parse(format!("bytes:{}", digest.as_ref()))
    }

    pub fn manifest(digest: impl AsRef<str>) -> Result<Self, CaptureError> {
        Self::parse(format!("manifest:{}", digest.as_ref()))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, CaptureError> {
        let value = value.into();
        let digest = value
            .strip_prefix("bytes:")
            .or_else(|| value.strip_prefix("manifest:"))
            .ok_or(CaptureError::PayloadAddress)?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CaptureError::PayloadAddress);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Physical immutable CAS key; kind remains part of every logical database key.
    pub fn digest(&self) -> &str {
        match self.kind() {
            PayloadKind::Bytes => &self.0[6..],
            PayloadKind::Manifest => &self.0[9..],
        }
    }

    pub fn kind(&self) -> PayloadKind {
        if self.0.starts_with("bytes:") {
            PayloadKind::Bytes
        } else {
            PayloadKind::Manifest
        }
    }
}

impl<'de> Deserialize<'de> for PayloadRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for PayloadRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for PayloadRef {
    type Err = CaptureError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// A server-issued authorization instance fixed at acquisition/command acceptance time.
///
/// It is a retention permission, not source identity. Deletion can retire this instance
/// without changing a native occurrence's position or an old receipt's disposition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct IngestionEpoch(String);

impl IngestionEpoch {
    pub fn new(value: impl Into<String>) -> Result<Self, TraceIdentityError> {
        let value = value.into();
        nonempty(&value, "ingestion authorization epoch")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IngestionEpoch {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for IngestionEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeUnit {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBasis {
    SourceWall,
    SourceMonotonic,
    ServerReceipt,
    ReportedDuration,
}

/// A quantity carries its own clock meaning. Missing evidence is represented by absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeEvidence {
    pub domain: String,
    pub value: i64,
    pub unit: TimeUnit,
    /// Resolution in the same unit as the value, when known.
    pub precision: Option<u64>,
    pub basis: TimeBasis,
}

impl TimeEvidence {
    pub fn validate(&self) -> Result<(), CaptureError> {
        nonempty(&self.domain, "clock domain")?;
        if self.precision == Some(0) {
            return Err(CaptureError::Clock("precision must be positive when known"));
        }
        if self.value < 0
            && matches!(
                self.basis,
                TimeBasis::SourceMonotonic | TimeBasis::ReportedDuration
            )
        {
            return Err(CaptureError::Clock(
                "monotonic values and reported durations cannot be negative",
            ));
        }
        Ok(())
    }

    /// Elapsed time in the endpoints' declared unit. No wall/receipt substitution or
    /// absolute-value repair is permitted; the caller exposes incompatible evidence.
    pub fn elapsed_until(&self, end: &Self) -> Result<i64, CaptureError> {
        self.validate()?;
        end.validate()?;
        if self.domain != end.domain || self.unit != end.unit || self.basis != end.basis {
            return Err(CaptureError::Clock(
                "endpoints use incompatible domains, units or bases",
            ));
        }
        if matches!(
            self.basis,
            TimeBasis::ServerReceipt | TimeBasis::ReportedDuration
        ) {
            return Err(CaptureError::Clock(
                "receipt time and reported duration are not execution endpoints",
            ));
        }
        end.value
            .checked_sub(self.value)
            .filter(|duration| *duration >= 0)
            .ok_or(CaptureError::Clock(
                "endpoints are reversed, reset or exceed the numeric range",
            ))
    }
}

impl<'de> Deserialize<'de> for TimeEvidence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            domain: String,
            value: i64,
            unit: TimeUnit,
            precision: Option<u64>,
            basis: TimeBasis,
        }
        let Wire {
            domain,
            value,
            unit,
            precision,
            basis,
        } = Wire::deserialize(deserializer)?;
        let value = Self {
            domain,
            value,
            unit,
            precision,
            basis,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadEncoding {
    Binary,
    Utf8,
    Base64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PayloadRange {
    pub start: u64,
    pub end_exclusive: u64,
}

impl PayloadRange {
    fn validate(&self) -> Result<(), CaptureError> {
        if self.start >= self.end_exclusive {
            Err(CaptureError::Manifest(
                "available byte ranges must be nonempty",
            ))
        } else {
            Ok(())
        }
    }
}

/// A manifest's completeness describes source evidence, never a requesting Subject's grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PayloadCompleteness {
    Complete,
    KnownRanges { ranges: Vec<PayloadRange> },
    Truncated { reason: String },
    SourceRedacted { reason: String },
    Opaque { reason: String },
    Unavailable { reason: String },
}

/// Byte ranges can reuse immutable chunks without flattening snapshot replacements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PayloadChunk {
    pub digest: PayloadRef,
    pub chunk_offset: u64,
    pub length: u64,
    pub payload_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadManifest {
    pub chunks: Vec<PayloadChunk>,
    pub media_type: String,
    pub encoding: PayloadEncoding,
    pub decoded_length: Option<u64>,
    pub completeness: PayloadCompleteness,
}

impl PayloadManifest {
    pub fn validate(&self) -> Result<(), CaptureError> {
        nonempty(&self.media_type, "payload media type")?;
        let mut covered_end = 0;
        for chunk in &self.chunks {
            if chunk.digest.kind() != PayloadKind::Bytes {
                return Err(CaptureError::Manifest(
                    "a chunk mapping must reference exact bytes, not another manifest",
                ));
            }
            if chunk.length == 0 {
                return Err(CaptureError::Manifest(
                    "zero-length chunk mappings are not allowed",
                ));
            }
            chunk
                .chunk_offset
                .checked_add(chunk.length)
                .ok_or(CaptureError::Manifest("chunk range overflow"))?;
            let end = chunk
                .payload_offset
                .checked_add(chunk.length)
                .ok_or(CaptureError::Manifest("payload range overflow"))?;
            if chunk.payload_offset < covered_end {
                return Err(CaptureError::Manifest(
                    "chunk mappings overlap or are out of order",
                ));
            }
            if matches!(self.completeness, PayloadCompleteness::Complete)
                && chunk.payload_offset != covered_end
            {
                return Err(CaptureError::Manifest("complete payload has a byte gap"));
            }
            if self.decoded_length.is_some_and(|length| end > length) {
                return Err(CaptureError::Manifest(
                    "chunk mapping exceeds declared decoded length",
                ));
            }
            covered_end = end;
        }
        match &self.completeness {
            PayloadCompleteness::Complete => {
                if self.decoded_length != Some(covered_end) {
                    return Err(CaptureError::Manifest(
                        "complete payload must declare its exact decoded length",
                    ));
                }
            }
            PayloadCompleteness::KnownRanges { ranges } => {
                if ranges.is_empty() {
                    return Err(CaptureError::Manifest(
                        "known ranges must name retained bytes",
                    ));
                }
                let mut previous_end = 0;
                for range in ranges {
                    range.validate()?;
                    if range.start < previous_end {
                        return Err(CaptureError::Manifest(
                            "known ranges overlap or are out of order",
                        ));
                    }
                    if self
                        .decoded_length
                        .is_some_and(|length| range.end_exclusive > length)
                    {
                        return Err(CaptureError::Manifest(
                            "known range exceeds declared decoded length",
                        ));
                    }
                    previous_end = range.end_exclusive;
                }
                // Compare the available bytes, not storage segment boundaries. Both
                // inputs have already been checked for order, overlap and overflow,
                // so coalescing adjacent extents makes this linear in their lengths.
                let declared = merged_ranges(ranges.iter().copied());
                let retained = merged_ranges(self.chunks.iter().map(|chunk| PayloadRange {
                    start: chunk.payload_offset,
                    end_exclusive: chunk.payload_offset + chunk.length,
                }));
                if declared != retained {
                    return Err(CaptureError::Manifest(
                        "declared available ranges differ from retained chunk mappings",
                    ));
                }
            }
            PayloadCompleteness::Unavailable { reason } => {
                nonempty(reason, "unavailable payload reason")?;
                if !self.chunks.is_empty() {
                    return Err(CaptureError::Manifest(
                        "unavailable payload cannot claim retained chunks",
                    ));
                }
            }
            PayloadCompleteness::Truncated { reason }
            | PayloadCompleteness::SourceRedacted { reason }
            | PayloadCompleteness::Opaque { reason } => {
                nonempty(reason, "payload coverage reason")?;
            }
        }
        Ok(())
    }
}

fn merged_ranges(ranges: impl Iterator<Item = PayloadRange>) -> Vec<PayloadRange> {
    let mut merged: Vec<PayloadRange> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut() {
            if previous.end_exclusive == range.start {
                previous.end_exclusive = range.end_exclusive;
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

impl<'de> Deserialize<'de> for PayloadManifest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            chunks: Vec<PayloadChunk>,
            media_type: String,
            encoding: PayloadEncoding,
            decoded_length: Option<u64>,
            completeness: PayloadCompleteness,
        }
        let Wire {
            chunks,
            media_type,
            encoding,
            decoded_length,
            completeness,
        } = Wire::deserialize(deserializer)?;
        let value = Self {
            chunks,
            media_type,
            encoding,
            decoded_length,
            completeness,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttachmentAvailability {
    Captured,
    Missing,
    Pending,
    Unreadable { reason: String },
    Partial { reason: String },
}

/// A mutable locator's observations remain independent of the bytes eventually retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureAttachment {
    pub locator: String,
    pub source_position: Option<SourcePosition>,
    pub observed_version: Option<String>,
    pub payload_ref: Option<PayloadRef>,
    pub availability: AttachmentAvailability,
    pub relation: String,
}

impl CaptureAttachment {
    pub fn validate(&self) -> Result<(), CaptureError> {
        nonempty(&self.locator, "attachment locator")?;
        nonempty(&self.relation, "attachment relation")?;
        if let Some(position) = &self.source_position {
            position.validate()?;
        }
        if let Some(version) = &self.observed_version {
            nonempty(version, "attachment observed version")?;
        }
        match &self.availability {
            AttachmentAvailability::Captured if self.payload_ref.is_none() => Err(
                CaptureError::Attachment("captured attachment requires a payload reference"),
            ),
            AttachmentAvailability::Missing | AttachmentAvailability::Pending
                if self.payload_ref.is_some() =>
            {
                Err(CaptureError::Attachment(
                    "missing or pending attachment cannot claim retained bytes",
                ))
            }
            AttachmentAvailability::Unreadable { reason } => {
                nonempty(reason, "unreadable attachment reason")?;
                if self.payload_ref.is_some() {
                    Err(CaptureError::Attachment(
                        "unreadable attachment cannot claim retained bytes",
                    ))
                } else {
                    Ok(())
                }
            }
            AttachmentAvailability::Partial { reason } => {
                nonempty(reason, "partial attachment reason")?;
                if self.payload_ref.is_none() {
                    Err(CaptureError::Attachment(
                        "partial attachment requires a manifest of retained ranges",
                    ))
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    LiveStdout,
    LiveStderr,
    NativeJsonl,
    NativeSnapshot,
    NativeDatabase,
    Sidecar,
    Control,
    Supervisor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureDiagnosticCode {
    MalformedRecord,
    IncompleteRecord,
    MissingSource,
    SourceChanged,
    MissingSidecar,
    UnsupportedEncoding,
    ResourceLimit,
    IoFailure,
    CaptureGap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureDiagnostic {
    pub code: CaptureDiagnosticCode,
    pub message: String,
}

/// Complete lexical bytes are retained through payload_ref before any semantic mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub observation_ref: ObservationRef,
    pub source_position: SourcePosition,
    pub ingestion_authorization_epoch: IngestionEpoch,
    pub declared_subject_refs: Vec<AgentRef>,
    pub captured_at: TimeEvidence,
    pub source_clock: Option<TimeEvidence>,
    pub transport_kind: TransportKind,
    pub vendor_build: Option<String>,
    pub schema_fingerprint: Option<String>,
    pub payload_ref: PayloadRef,
    pub attachments: Vec<CaptureAttachment>,
    pub capture_diagnostics: Vec<CaptureDiagnostic>,
}

impl Capture {
    pub fn validate(&self) -> Result<(), CaptureError> {
        self.observation_ref.validate()?;
        self.source_position.validate()?;
        self.captured_at.validate()?;
        if let Some(clock) = &self.source_clock {
            clock.validate()?;
        }
        if self.observation_ref.issuer != self.source_position.source.issuer {
            return Err(CaptureError::IssuerMismatch);
        }
        for subject in &self.declared_subject_refs {
            subject.validate()?;
            if let Issuer::Environment(environment) = &self.observation_ref.issuer {
                if subject.environment() != environment {
                    return Err(CaptureError::SubjectEnvironment);
                }
            }
        }
        for attachment in &self.attachments {
            attachment.validate()?;
        }
        for diagnostic in &self.capture_diagnostics {
            nonempty(&diagnostic.message, "capture diagnostic message")?;
        }
        if let Some(build) = &self.vendor_build {
            nonempty(build, "vendor build")?;
        }
        if let Some(fingerprint) = &self.schema_fingerprint {
            nonempty(fingerprint, "schema fingerprint")?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Capture {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            observation_ref: ObservationRef,
            source_position: SourcePosition,
            ingestion_authorization_epoch: IngestionEpoch,
            declared_subject_refs: Vec<AgentRef>,
            captured_at: TimeEvidence,
            source_clock: Option<TimeEvidence>,
            transport_kind: TransportKind,
            vendor_build: Option<String>,
            schema_fingerprint: Option<String>,
            payload_ref: PayloadRef,
            attachments: Vec<CaptureAttachment>,
            capture_diagnostics: Vec<CaptureDiagnostic>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            observation_ref: wire.observation_ref,
            source_position: wire.source_position,
            ingestion_authorization_epoch: wire.ingestion_authorization_epoch,
            declared_subject_refs: wire.declared_subject_refs,
            captured_at: wire.captured_at,
            source_clock: wire.source_clock,
            transport_kind: wire.transport_kind,
            vendor_build: wire.vendor_build,
            schema_fingerprint: wire.schema_fingerprint,
            payload_ref: wire.payload_ref,
            attachments: wire.attachments,
            capture_diagnostics: wire.capture_diagnostics,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> PayloadRef {
        PayloadRef::bytes("a".repeat(64)).unwrap()
    }

    fn complete() -> PayloadManifest {
        PayloadManifest {
            chunks: vec![PayloadChunk {
                digest: digest(),
                chunk_offset: 2,
                length: 3,
                payload_offset: 0,
            }],
            media_type: "application/octet-stream".into(),
            encoding: PayloadEncoding::Binary,
            decoded_length: Some(3),
            completeness: PayloadCompleteness::Complete,
        }
    }

    #[test]
    fn complete_payload_cannot_hide_missing_bytes_or_an_unknown_length() {
        let mut manifest = complete();
        assert!(manifest.validate().is_ok());
        manifest.chunks[0].payload_offset = 1;
        manifest.decoded_length = Some(4);
        assert_eq!(
            manifest.validate(),
            Err(CaptureError::Manifest("complete payload has a byte gap"))
        );
        manifest = complete();
        manifest.decoded_length = None;
        assert!(manifest.validate().is_err());
        manifest = complete();
        manifest.chunks.clear();
        manifest.decoded_length = Some(0);
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn known_payload_ranges_require_actual_retained_chunk_coverage() {
        let mut manifest = complete();
        manifest.completeness = PayloadCompleteness::KnownRanges {
            ranges: vec![PayloadRange {
                start: 0,
                end_exclusive: 4,
            }],
        };
        manifest.decoded_length = None;
        assert_eq!(
            manifest.validate(),
            Err(CaptureError::Manifest(
                "declared available ranges differ from retained chunk mappings"
            ))
        );
        manifest.completeness = PayloadCompleteness::KnownRanges {
            ranges: vec![PayloadRange {
                start: 0,
                end_exclusive: 3,
            }],
        };
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn declared_available_ranges_cannot_hide_retained_prefix_bytes() {
        let mut manifest = complete();
        manifest.completeness = PayloadCompleteness::KnownRanges {
            ranges: vec![PayloadRange {
                start: 1,
                end_exclusive: 3,
            }],
        };
        // The mapping exposes payload bytes 0..3, while availability claims only
        // 1..3. A reader must never return byte zero under that declaration.
        assert!(manifest.validate().is_err());

        // Segment boundaries are storage choices. Adjacent ranges and chunks
        // can partition the same available bytes differently.
        manifest.chunks = vec![
            PayloadChunk {
                digest: digest(),
                chunk_offset: 0,
                length: 2,
                payload_offset: 0,
            },
            PayloadChunk {
                digest: digest(),
                chunk_offset: 0,
                length: 1,
                payload_offset: 2,
            },
        ];
        manifest.completeness = PayloadCompleteness::KnownRanges {
            ranges: vec![
                PayloadRange {
                    start: 0,
                    end_exclusive: 1,
                },
                PayloadRange {
                    start: 1,
                    end_exclusive: 3,
                },
            ],
        };
        assert!(manifest.validate().is_ok());

        manifest.chunks[0].length = 1;
        manifest.completeness = PayloadCompleteness::KnownRanges {
            ranges: vec![
                PayloadRange {
                    start: 0,
                    end_exclusive: 1,
                },
                PayloadRange {
                    start: 2,
                    end_exclusive: 3,
                },
            ],
        };
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn sidecar_absence_cannot_masquerade_as_a_complete_empty_payload() {
        let mut attachment = CaptureAttachment {
            locator: "output.txt".into(),
            source_position: None,
            observed_version: None,
            payload_ref: None,
            availability: AttachmentAvailability::Missing,
            relation: "tool output".into(),
        };
        assert!(attachment.validate().is_ok());
        attachment.availability = AttachmentAvailability::Captured;
        assert!(attachment.validate().is_err());
        attachment.payload_ref = Some(digest());
        assert!(attachment.validate().is_ok());
        attachment.availability = AttachmentAvailability::Missing;
        assert!(attachment.validate().is_err());
    }

    #[test]
    fn incompatible_receipt_and_reported_clocks_never_supply_execution_elapsed_time() {
        let start = TimeEvidence {
            domain: "process-a".into(),
            value: 10,
            unit: TimeUnit::Seconds,
            precision: Some(1),
            basis: TimeBasis::SourceWall,
        };
        let mut end = TimeEvidence {
            value: 25,
            ..start.clone()
        };
        assert_eq!(start.elapsed_until(&end).unwrap(), 15);
        end.basis = TimeBasis::ServerReceipt;
        assert!(start.elapsed_until(&end).is_err());
        end = TimeEvidence {
            value: 5,
            ..start.clone()
        };
        assert!(start.elapsed_until(&end).is_err());
        end = TimeEvidence {
            domain: "process-b".into(),
            ..start.clone()
        };
        assert!(start.elapsed_until(&end).is_err());
        let duration = TimeEvidence {
            basis: TimeBasis::ReportedDuration,
            value: 100,
            ..start
        };
        assert!(duration.elapsed_until(&duration).is_err());
    }

    #[test]
    fn payload_and_manifest_wire_decoding_reject_invalid_digests_and_incomplete_claims() {
        assert!(PayloadRef::bytes("A".repeat(64)).is_err());
        assert!(serde_json::from_value::<PayloadRef>(serde_json::json!("not-a-digest")).is_err());
        let mut manifest = serde_json::to_value(complete()).unwrap();
        manifest["decodedLength"] = serde_json::json!(4);
        assert!(serde_json::from_value::<PayloadManifest>(manifest).is_err());
        assert!(serde_json::from_value::<IngestionEpoch>(serde_json::json!("")).is_err());
    }

    #[test]
    fn identical_cas_bytes_have_distinct_raw_and_manifest_addresses() {
        // The physical object can be both a manifest encoding and a vendor's exact
        // arbitrary output. Its digest is intentionally identical in this case.
        let physical_digest = "a".repeat(64);
        let raw = PayloadRef::bytes(&physical_digest).unwrap();
        let manifest = PayloadRef::manifest(&physical_digest).unwrap();
        assert_ne!(raw.as_str(), manifest.as_str());
        assert_eq!(raw.digest(), physical_digest);
        assert_eq!(manifest.digest(), physical_digest);
        assert_eq!(raw.kind(), PayloadKind::Bytes);
        assert_eq!(manifest.kind(), PayloadKind::Manifest);
        assert!(PayloadRef::parse(&physical_digest).is_err());
        assert_eq!(
            serde_json::from_value::<PayloadRef>(serde_json::json!(format!(
                "manifest:{physical_digest}"
            )))
            .unwrap(),
            manifest
        );
        let mut nested = complete();
        nested.chunks[0].digest = manifest;
        assert_eq!(
            nested.validate(),
            Err(CaptureError::Manifest(
                "a chunk mapping must reference exact bytes, not another manifest"
            ))
        );
    }
}
