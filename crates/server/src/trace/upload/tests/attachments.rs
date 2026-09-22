use super::*;
use sylph_protocol::trace::{AttachmentAvailability, CaptureAttachment};

const RELATION: &[u8] = b"retained sidecar relation";

#[tokio::test]
async fn sidecar_proof_accepts_every_admitted_primary_representation_of_the_same_source() {
    let fixture = Fixture::new().await;
    let raw = b"same retained source bytes";
    let source = admit_bytes(&fixture, 1, raw).await;
    let manifest = PayloadManifest {
        chunks: vec![PayloadChunk {
            digest: source.payload_ref.clone(),
            chunk_offset: 0,
            length: raw.len() as u64,
            payload_offset: 0,
        }],
        media_type: "application/octet-stream".into(),
        encoding: PayloadEncoding::Binary,
        decoded_length: Some(raw.len() as u64),
        completeness: PayloadCompleteness::Complete,
    };
    let mut reread = source.clone();
    reread.observation_ref.producer_seq = 2;
    reread.payload_ref = crate::trace::evidence::manifest_ref(&manifest).unwrap();
    let BeginCapture::Pending(ticket) = fixture
        .uploads
        .begin(
            &reread.observation_ref.issuer,
            reread.clone(),
            vec![
                PayloadDeclaration::Manifest {
                    reference: reread.payload_ref.clone(),
                    manifest,
                },
                PayloadDeclaration::Bytes {
                    reference: source.payload_ref.clone(),
                    length: raw.len() as u64,
                },
            ],
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    fixture
        .uploads
        .finish(&reread.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    let page = fixture.evidence.admissions_after(0, 10).await.unwrap();
    assert_eq!(
        page.captures[0].occurrence_id,
        page.captures[1].occurrence_id
    );
    let capture = relation(&fixture, &reread, 3);
    let ticket = fixture.ticket(&capture, RELATION).await;
    fixture
        .uploads
        .put_chunk(
            &capture.observation_ref.issuer,
            &ticket,
            &capture.payload_ref,
            RELATION,
        )
        .await
        .unwrap();
    fixture
        .uploads
        .finish(&capture.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .evidence
            .payload_bytes(&reread.payload_ref, 100)
            .await
            .unwrap(),
        raw
    );
}

async fn admit_bytes(fixture: &Fixture, sequence: u64, bytes: &[u8]) -> Capture {
    let capture = fixture.capture(sequence, sequence, bytes);
    let ticket = fixture.ticket(&capture, bytes).await;
    fixture
        .uploads
        .put_chunk(
            &capture.observation_ref.issuer,
            &ticket,
            &capture.payload_ref,
            bytes,
        )
        .await
        .unwrap();
    fixture
        .uploads
        .finish(&capture.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    capture
}

fn relation(fixture: &Fixture, source: &Capture, sequence: u64) -> Capture {
    let mut capture = fixture.capture(sequence, sequence, RELATION);
    capture.transport_kind = TransportKind::SourceAttachment;
    capture.attachments.push(CaptureAttachment {
        locator: "/vendor/already-removed.json".into(),
        source_position: Some(source.source_position.clone()),
        observed_version: Some(source.source_position.generation.to_string()),
        payload_ref: Some(source.payload_ref.clone()),
        availability: AttachmentAvailability::Captured,
        relation: "tool_output".into(),
    });
    capture
}

#[tokio::test]
async fn retained_sidecar_manifest_proof_survives_restart_without_reuploading_its_chunks() {
    let mut fixture = Fixture::new().await;
    let chunks = [b"first\r\n".as_slice(), b"second\0".as_slice()];
    let mut offset = 0;
    let manifest = PayloadManifest {
        chunks: chunks
            .iter()
            .map(|bytes| {
                let chunk = PayloadChunk {
                    digest: PayloadRef::bytes(blake3::hash(bytes).to_hex()).unwrap(),
                    chunk_offset: 0,
                    length: bytes.len() as u64,
                    payload_offset: offset,
                };
                offset += bytes.len() as u64;
                chunk
            })
            .collect(),
        media_type: "application/octet-stream".into(),
        encoding: PayloadEncoding::Binary,
        decoded_length: Some(chunks.iter().map(|bytes| bytes.len() as u64).sum()),
        completeness: PayloadCompleteness::Complete,
    };
    let mut source = fixture.capture(1, 1, b"source");
    source.payload_ref = crate::trace::evidence::manifest_ref(&manifest).unwrap();
    let mut declarations = vec![PayloadDeclaration::Manifest {
        reference: source.payload_ref.clone(),
        manifest: manifest.clone(),
    }];
    declarations.extend(
        manifest
            .chunks
            .iter()
            .map(|chunk| PayloadDeclaration::Bytes {
                reference: chunk.digest.clone(),
                length: chunk.length,
            }),
    );
    let BeginCapture::Pending(ticket) = fixture
        .uploads
        .begin(&source.observation_ref.issuer, source.clone(), declarations)
        .await
        .unwrap()
    else {
        panic!()
    };
    for (chunk, bytes) in manifest.chunks.iter().zip(chunks) {
        fixture
            .uploads
            .put_chunk(
                &source.observation_ref.issuer,
                &ticket,
                &chunk.digest,
                bytes,
            )
            .await
            .unwrap();
    }
    fixture
        .uploads
        .finish(&source.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    fixture.reopen().await;
    let capture = relation(&fixture, &source, 2);
    let ticket = fixture.ticket(&capture, RELATION).await;
    // Only the new relation's bytes are transferred. The immutable source
    // occurrence proves the manifest and both sidecar chunks independently.
    fixture
        .uploads
        .put_chunk(
            &capture.observation_ref.issuer,
            &ticket,
            &capture.payload_ref,
            RELATION,
        )
        .await
        .unwrap();
    fixture.reopen().await;
    let receipt = fixture
        .uploads
        .finish(&capture.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    assert_eq!(receipt.accepted_through, 2);
    assert_eq!(
        fixture
            .evidence
            .payload_bytes(&source.payload_ref, 100)
            .await
            .unwrap(),
        b"first\r\nsecond\0"
    );
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&capture.observation_ref, 100)
            .await
            .unwrap(),
        Some(RELATION.to_vec())
    );
    let repeated = fixture
        .uploads
        .begin(&capture.observation_ref.issuer, capture.clone(), vec![])
        .await
        .unwrap();
    assert!(
        matches!(repeated, BeginCapture::Complete(batch) if batch.accepted_through == 2 && !batch.receipts[0].newly_recorded)
    );
    // Once the proof is received, physical collection must still honor its roots.
    let collector =
        TraceGarbageCollector::new(fixture.uploads.catalogue().clone(), GcLimits::default())
            .unwrap();
    let progress = collector.run_once(1000).await.unwrap();
    assert!(progress.inspected > 0);
    assert_eq!(progress.collected, 0);
    assert_eq!(
        fixture
            .evidence
            .payload_bytes(&source.payload_ref, 100)
            .await
            .unwrap(),
        b"first\r\nsecond\0"
    );
}

#[tokio::test]
async fn sidecar_reuse_requires_the_exact_issuer_epoch_position_and_primary_payload() {
    let fixture = Fixture::new().await;
    let source = admit_bytes(&fixture, 1, b"sidecar secret").await;
    let unrelated = admit_bytes(&fixture, 2, b"other known bytes").await;
    let other_agent = AgentRef::Session(SessionId::decode("env7fk2:codex:other").unwrap());
    let other_epoch = fixture
        .evidence
        .authorize_scope(
            &IngestionScope::Agent(other_agent.clone()),
            std::slice::from_ref(&other_agent),
        )
        .await
        .unwrap();
    for fault in [
        "issuer",
        "epoch",
        "source",
        "generation",
        "locator",
        "payload",
        "missing_position",
    ] {
        let mut capture = relation(&fixture, &source, 3);
        let attachment = &mut capture.attachments[0];
        match fault {
            "issuer" => {
                attachment.source_position.as_mut().unwrap().source.issuer =
                    Issuer::Server("foreign".into())
            }
            "epoch" => {
                capture.ingestion_authorization_epoch = other_epoch.clone();
                capture.declared_subject_refs = vec![other_agent.clone()];
            }
            "source" => {
                attachment.source_position.as_mut().unwrap().source.instance = "uncaptured".into()
            }
            "generation" => attachment.source_position.as_mut().unwrap().generation += 1,
            "locator" => {
                attachment.source_position.as_mut().unwrap().locator = SourceLocator::Occurrence {
                    key: "unobserved".into(),
                }
            }
            "payload" => attachment.payload_ref = Some(unrelated.payload_ref.clone()),
            "missing_position" => attachment.source_position = None,
            _ => unreachable!(),
        }
        let result = fixture
            .uploads
            .begin(
                &capture.observation_ref.issuer,
                capture.clone(),
                vec![PayloadDeclaration::Bytes {
                    reference: capture.payload_ref.clone(),
                    length: RELATION.len() as u64,
                }],
            )
            .await;
        assert!(
            matches!(result, Err(UploadError::Incomplete)),
            "{fault}: {result:?}"
        );
        assert!(fixture
            .evidence
            .receipt(&capture.observation_ref)
            .await
            .unwrap()
            .is_none());
    }
    assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), 2);
}

#[tokio::test]
async fn deleting_a_sidecar_epoch_discards_pending_relations_and_cannot_supply_readoption_proof() {
    let mut fixture = Fixture::new().await;
    let source = admit_bytes(&fixture, 1, b"retained sidecar").await;
    let capture = relation(&fixture, &source, 2);
    let ticket = fixture.ticket(&capture, RELATION).await;
    fixture
        .uploads
        .put_chunk(
            &capture.observation_ref.issuer,
            &ticket,
            &capture.payload_ref,
            RELATION,
        )
        .await
        .unwrap();
    fixture
        .evidence
        .delete_scope(&IngestionScope::Agent(fixture.agent.clone()))
        .await
        .unwrap();
    let batch = fixture
        .uploads
        .finish(&capture.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    assert_eq!(batch.accepted_through, 2);
    assert!(matches!(
        batch.receipts[0].disposition,
        crate::trace::evidence::ReceiptDisposition::DiscardedByDeletion { .. }
    ));
    fixture.epoch = fixture
        .evidence
        .readopt_scope(
            &IngestionScope::Agent(fixture.agent.clone()),
            std::slice::from_ref(&fixture.agent),
        )
        .await
        .unwrap();
    let fresh = relation(&fixture, &source, 3);
    assert!(matches!(
        fixture
            .uploads
            .begin(
                &fresh.observation_ref.issuer,
                fresh.clone(),
                vec![PayloadDeclaration::Bytes {
                    reference: fresh.payload_ref.clone(),
                    length: RELATION.len() as u64
                }]
            )
            .await,
        Err(UploadError::Incomplete)
    ));
}

#[tokio::test]
async fn sidecar_proof_is_rechecked_inside_admission_instead_of_trusting_the_upload_ticket() {
    let fixture = Fixture::new().await;
    let source = admit_bytes(&fixture, 1, b"retained sidecar").await;
    let capture = relation(&fixture, &source, 2);
    let ticket = fixture.ticket(&capture, RELATION).await;
    fixture
        .uploads
        .put_chunk(
            &capture.observation_ref.issuer,
            &ticket,
            &capture.payload_ref,
            RELATION,
        )
        .await
        .unwrap();
    // Simulate a corrupted durable source index after ticket creation. All CAS
    // bytes and the completed upload survive, so only final proof validation
    // can prevent admission from certifying a source that no longer matches.
    use sqlx::Connection;
    let mut conn = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(fixture.dir.path().join("evidence.db")),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE occurrences SET locator_json=?")
        .bind(
            serde_json::to_string(&SourceLocator::Occurrence {
                key: "corrupted-index".into(),
            })
            .unwrap(),
        )
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    assert!(
        matches!(fixture.uploads.finish(&capture.observation_ref.issuer, &ticket).await, Err(UploadError::Evidence(EvidenceError::Invalid(reason))) if reason.contains("source occurrence"))
    );
    assert!(fixture
        .evidence
        .receipt(&capture.observation_ref)
        .await
        .unwrap()
        .is_none());
}
