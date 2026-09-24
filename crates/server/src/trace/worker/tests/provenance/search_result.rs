use super::*;

async fn request(
    f: &ProvenanceFixture,
    method: &str,
    path: &str,
    cookie: &str,
) -> axum::response::Response {
    f.app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "localhost")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn search_results_open_superseded_composed_bytes_with_exact_authority_and_independent_derivative_jobs(
) {
    let mut f = ProvenanceFixture::new().await;
    let indexer = f.state.trace.fixture_indexer().await;
    let owner = agent("recipient", "search-body");
    let message = MessageRef::native(owner.clone(), "search-body", vec!["one".into()]).unwrap();
    let mut captures = vec![];
    let mut claims = vec![];
    let mut revisions: Vec<MessageRevisionRef> = vec![];
    let mut entries = vec![];
    for (index, text) in [r#"{"old":"#, r#""matched"}"#, r#"{"new":"current"}"#]
        .into_iter()
        .enumerate()
    {
        let source = f
            .capture(&owner, &format!("segment-{index}"), text.as_bytes())
            .await;
        if index == 0 {
            claims.push(claim(
                &owner,
                "agent",
                agent_object(&owner, "Exact result"),
                &[&source],
            ));
            claims.push(claim(
                &owner,
                "message",
                TraceObject::Message(Message {
                    message_ref: message.clone(),
                    author: Author::Agent {
                        agent: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                    },
                    protocol_role: Some(ProtocolRole::Assistant),
                    purpose: MessagePurpose::Answer,
                    production: Some(ProductionScope {
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                        activation: None,
                    }),
                }),
                &[&source],
            ));
        }
        let entry =
            EntryRef::native(owner.clone(), "search-body", vec![index.to_string()]).unwrap();
        let revision =
            MessageRevisionRef::native(owner.clone(), "search-body", vec![index.to_string()])
                .unwrap();
        let form = match index {
            0 => RevisionForm::Append { predecessor: None },
            1 => RevisionForm::Append {
                predecessor: Some(revisions[0].clone()),
            },
            _ => RevisionForm::Correction {
                replaces: revisions[1].clone(),
            },
        };
        claims.push(claim(
            &owner,
            &format!("entry-{index}"),
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: owner.clone(),
                predecessor: entries.last().cloned(),
                original_owner: None,
            }),
            &[&source],
        ));
        claims.push(claim(
            &owner,
            &format!("revision-{index}"),
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message: message.clone(),
                representation: Representation::Conversation,
                form,
                parts: vec![ContentPart {
                    part_ref: MessagePartRef::native(
                        owner.clone(),
                        "search-body",
                        vec!["part".into()],
                    )
                    .unwrap(),
                    kind: ContentKind::Structured,
                    payload: source.payload_ref.clone(),
                    media_type: Some("application/json".into()),
                    encoding: PayloadEncoding::Utf8,
                }],
            }),
            &[&source],
        ));
        let mut observed = claim(
            &owner,
            &format!("content-{index}"),
            agent_object(&owner, "unused"),
            &[&source],
        );
        observed.fact = Fact::HistoryContentObserved {
            observation: HistoryContentObservation::complete(
                entry.clone(),
                source.source_position.clone(),
                "search-body",
                vec![HistoryContent::Message(revision.clone())],
            ),
        };
        claims.push(observed);
        f.committed.previews.insert(
            source.payload_ref.clone(),
            Arc::new(sylph_trace::reading::PayloadPreview {
                preview: text.into(),
                preview_limit: 2048,
                total_bytes: Some(text.len() as u64),
                ranges: vec![PayloadRange {
                    start: 0,
                    end_exclusive: text.len() as u64,
                }],
                availability: Some(ReadAvailability::Available),
                ..Default::default()
            }),
        );
        entries.push(entry);
        revisions.push(revision);
        captures.push(source);
    }
    f.publish(claims, &captures.iter().collect::<Vec<_>>())
        .await;
    indexer.index_pending().await.unwrap();
    let (status, found) = f
        .get(&format!(
            "/api/v1/trace/search?agent={}&q=matched",
            owner.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    let hit = &found["items"][0];
    let access = &hit["payload"];
    assert_eq!(access["bodyRevision"], revisions[1].encode());
    assert_eq!(access["mediaType"], "application/json");
    assert_eq!(access["authority"]["kind"], "search_result");
    let token = found["view"]["token"].as_str().unwrap();
    assert_eq!(access["totalBytes"], br#"{"old":"matched"}"#.len());
    let (status, context) = f
        .get(&format!(
            "/api/v1/trace/agents/{}?archive=true&around={}&at_snapshot={token}",
            owner.encode(),
            hit["anchor"].as_str().unwrap()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{context}");
    assert!(context.to_string().contains("current"), "{context}");
    let body_ref = access["payloadRef"].as_str().unwrap();
    let ticket = access["authority"]["resultRef"].as_str().unwrap();
    let plain = format!(
        "at_snapshot={token}&via_ref={}&revision={}&representation=conversation",
        access["viaRef"].as_str().unwrap(),
        revisions[1].encode()
    );
    let params = format!("{plain}&search_result={ticket}");
    let path = format!("/api/v1/trace/payloads/{body_ref}");
    assert_eq!(
        f.get_response(&format!("{path}?{plain}")).await.status(),
        StatusCode::FORBIDDEN,
        "the fixture must require the search authority for this historical composition"
    );
    let bytes = f.get_response(&format!("{path}?{params}")).await;
    assert_eq!(bytes.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(bytes.into_body(), 1024).await.unwrap(),
        br#"{"old":"matched"}"#.as_slice()
    );
    let copied = f
        .get_response(&format!("{path}/transfer?{params}&kind=copy"))
        .await;
    assert_eq!(copied.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(copied.into_body(), 1024)
            .await
            .unwrap(),
        br#"{"old":"matched"}"#.as_slice()
    );
    for wrong in [
        params.replace(&revisions[1].encode(), &revisions[0].encode()),
        params.replace("representation=conversation", "representation=runtime"),
    ] {
        assert_eq!(
            f.get_response(&format!("{path}?{wrong}")).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    let (cookie, subject) = register(&f.app, "other-search-reader").await;
    f.state
        .store
        .issue_grant(&subject, "recipient", "observe", &f.subject, None)
        .await
        .unwrap();
    let other_view = f.state.trace.lease_guard(&subject, None).await.unwrap();
    let foreign_params = params.replace(token, &other_view.token);
    assert_eq!(
        request(&f, "GET", &format!("{path}?{foreign_params}"), &cookie)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    let (status, created) =
        response(request(&f, "POST", &format!("{path}/structure?{params}"), &f.cookie).await).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{created}");
    let structure = format!(
        "/api/v1/trace/structures/{}",
        created["id"].as_str().unwrap()
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (status, page) = f.get(&structure).await;
            assert_eq!(status, StatusCode::OK, "{page}");
            if page["state"] != "building" {
                assert_eq!(page["state"], "ready", "{page}");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let newer = f.capture(&owner, "later", b"later evidence").await;
    f.publish(vec![], &[&newer]).await;
    let current = f.bootstrap(&owner).await;
    assert!(current.to_string().contains("current"), "{current}");
    let wrong_view = params.replace(token, current["view"]["token"].as_str().unwrap());
    assert_eq!(
        f.get_response(&format!("{path}?{wrong_view}"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    f.state
        .trace
        .search_store(&f.generation.manifest().generation)
        .await
        .unwrap()
        .invalidate()
        .await
        .unwrap();
    assert_eq!(
        f.get_response(&format!("{path}?{params}")).await.status(),
        StatusCode::GONE
    );
    let (status, retained) = f.get(&structure).await;
    assert_eq!(status, StatusCode::OK, "{retained}");
    assert!(retained.to_string().contains("matched"), "{retained}");
    f.state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(owner))
        .await
        .unwrap();
    assert!(!f.get_response(&structure).await.status().is_success());
}
