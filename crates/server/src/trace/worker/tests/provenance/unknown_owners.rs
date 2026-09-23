use super::*;

#[tokio::test]
async fn operation_input_is_readable_in_its_namespace_without_asserting_an_actor() {
    let mut fixture = ProvenanceFixture::new().await;
    let containing = agent("recipient", "history-reader");
    let body = br#"{"path":"retained.txt"}"#;
    let capture = fixture.capture(&containing, "historical-input", body).await;
    let mut claims = vec![claim(
        &containing,
        "agent",
        agent_object(&containing, "Reader"),
        &[&capture],
    )];
    let owners = [
        OwnerRef::Unknown,
        OwnerRef::Unresolved {
            scope: containing.clone(),
            namespace: "entry-origin".into(),
            native_key: vec!["copied".into()],
            reason: "Origin unavailable".into(),
        },
    ];
    let mut references = vec![];
    let mut predecessor = None;
    for (i, owner) in owners.into_iter().enumerate() {
        let key = format!("operation-{i}");
        let operation_ref =
            OperationRef::native(containing.clone(), "historical", vec![key.clone()]).unwrap();
        references.push(AnyObjectRef::Operation(operation_ref.clone()));
        let entry = EntryRef::native(containing.clone(), "history", vec![key.clone()]).unwrap();
        claims.push(claim(
            &containing,
            &format!("entry-{i}"),
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: containing.clone(),
                predecessor: predecessor.clone(),
                original_owner: None,
            }),
            &[&capture],
        ));
        let mut contents = claim(
            &containing,
            &format!("contents-{i}"),
            agent_object(&containing, "Reader"),
            &[&capture],
        );
        contents.fact = Fact::HistoryContentObserved {
            observation: HistoryContentObservation::complete(
                entry.clone(),
                capture.source_position.clone(),
                "retained-invocation",
                vec![HistoryContent::Operation(operation_ref.clone())],
            ),
        };
        claims.push(contents);
        predecessor = Some(entry);

        claims.push(claim(
            &containing,
            &key,
            TraceObject::Operation(Operation {
                operation_ref,
                owner,
                activation: None,
                requester: None,
                category: OperationCategory::Tool {
                    namespace: None,
                    name: "read".into(),
                },
                input_revision: Some(capture.payload_ref.clone()),
                argument_state: ArgumentState::Sealed,
            }),
            &[&capture],
        ));
    }
    fixture.publish(claims, &[&capture]).await;
    let bootstrap = fixture.bootstrap(&containing).await;
    let token = bootstrap["view"]["token"].as_str().unwrap();
    let rows = bootstrap["window"]["items"].as_array().unwrap();
    assert_eq!(rows.len(), references.len());
    for (row, reference) in rows.iter().zip(references) {
        assert_eq!(
            row["content"]["kind"], "work",
            "a retained invocation must remain inspectable"
        );
        assert!(row["content"]["owner"].is_null());
        assert!(row["activation"].is_null());
        assert_eq!(row["content"]["operationsAnchor"], reference.encode());
        let (status, detail) = fixture
            .get(&format!(
                "/api/v1/trace/objects/{}?at_snapshot={token}",
                row["content"]["operationsAnchor"].as_str().unwrap()
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert!(
            detail["owner"].is_null(),
            "containing namespace is not the producer"
        );
        assert_eq!(detail["execution"]["state"]["kind"], "unavailable");
        let input = &detail["content"]["input"][0];
        assert_eq!(input["payloadRef"], capture.payload_ref.to_string());
        let url = format!(
            "/api/v1/trace/payloads/{}?at_snapshot={token}&via_ref={}&revision={}&representation=runtime",
            input["payloadRef"].as_str().unwrap(),
            input["viaRef"].as_str().unwrap(),
            input["bodyRevision"].as_str().unwrap()
        );
        let response = fixture.get_response(&url).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), body);
    }
}
