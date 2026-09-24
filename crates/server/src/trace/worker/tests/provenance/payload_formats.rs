use super::*;

#[tokio::test]
async fn retained_manifest_format_reaches_an_unclassified_result_after_worker_restore() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "manifest-format");
    let chunk = fixture
        .capture(&owner, "manifest-chunk", b"retained text")
        .await;
    let manifest = PayloadManifest {
        chunks: vec![PayloadChunk {
            digest: chunk.payload_ref.clone(),
            chunk_offset: 0,
            payload_offset: 0,
            length: 13,
        }],
        media_type: "text/plain".into(),
        encoding: PayloadEncoding::Utf8,
        decoded_length: Some(13),
        completeness: PayloadCompleteness::Complete,
    };
    let mut capture = chunk.clone();
    capture.observation_ref.producer_id = "manifest-capture".into();
    capture.source_position.source.instance = "manifest-capture".into();
    capture.payload_ref = fixture
        .state
        .trace
        .evidence()
        .stage_manifest(&manifest)
        .await
        .unwrap();
    fixture
        .state
        .trace
        .evidence()
        .admit_batch(
            &capture.observation_ref.issuer,
            std::slice::from_ref(&capture),
        )
        .await
        .unwrap();
    let operation =
        OperationRef::native(owner.clone(), "operation", vec!["manifest".into()]).unwrap();
    fixture
        .publish(
            vec![
                claim(
                    &owner,
                    "agent",
                    agent_object(&owner, "Manifest reader"),
                    &[&capture],
                ),
                claim(
                    &owner,
                    "operation",
                    TraceObject::Operation(Operation {
                        operation_ref: operation.clone(),
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                        activation: None,
                        requester: None,
                        category: OperationCategory::Wait,
                        input_revision: None,
                        argument_state: ArgumentState::Sealed,
                    }),
                    &[&capture],
                ),
                claim(
                    &owner,
                    "result",
                    TraceObject::Result(ResultOccurrence {
                        result_ref: ResultRef::native(
                            owner.clone(),
                            "result",
                            vec!["manifest".into()],
                        )
                        .unwrap(),
                        operation: operation.clone(),
                        recipient: None,
                        disposition: ResultDisposition::FinalReturn,
                        representation: ResultRepresentation::Runtime,
                        payload: Some(PayloadUse::unclassified(capture.payload_ref.clone())),
                        delivery: None,
                    }),
                    &[&capture],
                ),
            ],
            &[&capture],
        )
        .await;
    fixture.committed = fixture
        .state
        .trace
        .worker()
        .load_state(&fixture.generation, fixture.committed.committed_snapshot())
        .await
        .unwrap()
        .0;
    fixture.publish(vec![], &[]).await;
    let bootstrap = fixture.bootstrap(&owner).await;
    let token = bootstrap["view"]["token"].as_str().unwrap();
    let (status, page) = fixture.get(&format!("/api/v1/trace/objects/{}/relations?at_snapshot={token}&kind=results&representation=runtime", AnyObjectRef::Operation(operation).encode())).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"][0]["payloads"][0]["mediaType"], "text/plain");
    assert_eq!(page["items"][0]["payloads"][0]["encoding"], "utf8");
}

#[tokio::test]
async fn equal_bytes_keep_each_payload_uses_format_through_projection_restart_and_http_reads() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "formats");
    let capture = fixture.capture(&owner, "same-bytes", b"{}").await;
    let other = agent("origin", "format-contributor");
    let unrelated = fixture.capture(&other, "unrelated-format", b"{}").await;
    let formats = [
        PayloadFormat {
            media_type: Some("application/json".into()),
            encoding: Some(PayloadEncoding::Utf8),
        },
        PayloadFormat {
            media_type: Some("text/plain".into()),
            encoding: Some(PayloadEncoding::Utf8),
        },
        PayloadFormat {
            media_type: Some("application/octet-stream".into()),
            encoding: Some(PayloadEncoding::Binary),
        },
        PayloadFormat::default(),
    ];
    let mut claims = vec![claim(
        &owner,
        "identity",
        agent_object(&owner, "Format reader"),
        &[&capture],
    )];
    let mut operations = vec![];
    for (index, format) in formats.iter().enumerate() {
        let key = format!("use-{index}");
        let operation =
            OperationRef::native(owner.clone(), "operation", vec![key.clone()]).unwrap();
        let payload = PayloadUse {
            reference: capture.payload_ref.clone(),
            format: format.clone(),
        };
        claims.push(claim(
            &owner,
            &key,
            TraceObject::Operation(Operation {
                operation_ref: operation.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
                activation: None,
                requester: None,
                category: OperationCategory::Wait,
                input_revision: Some(payload.clone()),
                argument_state: ArgumentState::Sealed,
            }),
            &[&capture],
        ));
        claims.push(claim(
            &owner,
            &format!("result-{index}"),
            TraceObject::Result(ResultOccurrence {
                result_ref: ResultRef::native(owner.clone(), "result", vec![key]).unwrap(),
                operation: operation.clone(),
                recipient: None,
                disposition: ResultDisposition::FinalReturn,
                representation: ResultRepresentation::CallerResult,
                payload: Some(payload),
                delivery: None,
            }),
            &[&capture],
        ));
        operations.push(operation);
    }
    fixture.publish(claims, &[&capture]).await;
    // Restore the committed worker cache rather than feeding it the same in-memory
    // declarations again. The empty continuation must preserve exact format uses.
    fixture.committed = fixture
        .state
        .trace
        .worker()
        .load_state(&fixture.generation, fixture.committed.committed_snapshot())
        .await
        .unwrap()
        .0;
    fixture.publish(vec![], &[]).await;
    let checkpoint = fixture.generation.checkpoint().await.unwrap();
    let committed = fixture
        .generation
        .apply(
            checkpoint.fence_token,
            &checkpoint.snapshot,
            &crate::trace::generation::ProjectionBatch {
                through_cut: checkpoint.snapshot.evidence_cut,
                continuation: true,
                payload_derivations: vec![PayloadDerivation {
                    payload: capture.payload_ref.clone(),
                    evidence: vec![unrelated.observation_ref.clone()],
                    selector: "/unrelated".into(),
                    rule: "unrelated-format".into(),
                    media_type: "text/html".into(),
                    encoding: PayloadEncoding::Utf8,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture.committed.snapshot = Some(committed.snapshot);
    fixture
        .state
        .store
        .revoke_grant(&fixture.subject, "origin", "observe")
        .await
        .unwrap();
    let bootstrap = fixture.bootstrap(&owner).await;
    let token = bootstrap["view"]["token"].as_str().unwrap();
    for (operation, format) in operations.iter().zip(formats) {
        let reference = AnyObjectRef::Operation(operation.clone()).encode();
        let (status, detail) = fixture
            .get(&format!(
                "/api/v1/trace/objects/{reference}?at_snapshot={token}"
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        let (status, results) = fixture.get(&format!("/api/v1/trace/objects/{reference}/relations?at_snapshot={token}&kind=results&representation=caller_result")).await;
        assert_eq!(status, StatusCode::OK, "{results}");
        for access in [
            &detail["content"]["input"][0],
            &results["items"][0]["payloads"][0],
        ] {
            assert_eq!(
                access["mediaType"],
                serde_json::to_value(&format.media_type).unwrap()
            );
            assert_eq!(
                access["encoding"],
                serde_json::to_value(format.encoding).unwrap()
            );
            assert_eq!(access["payloadRef"], capture.payload_ref.to_string());
            let response = fixture.get_response(&format!("/api/v1/trace/payloads/{}?at_snapshot={token}&via_ref={}&revision={}&representation={}&start=0&end=2", access["payloadRef"].as_str().unwrap(), access["viaRef"].as_str().unwrap(), access["bodyRevision"].as_str().unwrap(), access["representation"].as_str().unwrap())).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/octet-stream"
            );
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap()
                    .as_ref(),
                b"{}"
            );
        }
    }
}
