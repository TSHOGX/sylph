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
    for (i, owner) in owners.into_iter().enumerate() {
        let key = format!("operation-{i}");
        let operation_ref =
            OperationRef::native(containing.clone(), "historical", vec![key.clone()]).unwrap();
        references.push(AnyObjectRef::Operation(operation_ref.clone()));
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
    for reference in references {
        let (status, detail) = fixture
            .get(&format!(
                "/api/v1/trace/objects/{}?at_snapshot={token}",
                reference.encode()
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
