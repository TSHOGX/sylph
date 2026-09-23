use super::*;

#[tokio::test]
async fn directory_scope_includes_the_focused_agent_and_all_spawn_descendants_but_not_forks() {
    let mut fixture = ProvenanceFixture::new().await;
    let root = agent("recipient", "directory-root");
    let child = agent("recipient", "directory-child");
    let grandchild = agent("recipient", "directory-grandchild");
    let fork = agent("recipient", "directory-fork");
    let capture = fixture
        .capture(&root, "topology", b"retained topology evidence")
        .await;
    let mut claims: Vec<_> = [&root, &child, &grandchild, &fork]
        .into_iter()
        .map(|owner| {
            claim(
                owner,
                "identity",
                agent_object(owner, owner.anchor_session().native_id.as_str()),
                &[&capture],
            )
        })
        .collect();
    for (parent, child) in [(&root, &child), (&child, &grandchild)] {
        let mut edge = claim(
            parent,
            &format!("spawn-{}", child.encode()),
            agent_object(parent, "unused"),
            &[&capture],
        );
        edge.fact = Fact::Relation {
            relation: Relation::Spawn {
                parent: parent.clone(),
                child: child.clone(),
                operation: None,
            },
        };
        claims.push(edge);
    }
    let checkpoint =
        CheckpointRef::native(root.clone(), "fork-context", vec!["copy".into()]).unwrap();
    claims.push(claim(
        &root,
        "fork-checkpoint",
        TraceObject::ContextCheckpoint(ContextCheckpoint {
            checkpoint_ref: checkpoint.clone(),
            owner: OwnerRef::Agent {
                agent: root.clone(),
            },
            basis: ContextBasis::InheritedMembership,
            content: ContextContent::Unknown {
                reason: "Only lineage was observed".into(),
            },
            coverage: InclusionCoverage::Unknown {
                reason: "Membership unavailable".into(),
            },
        }),
        &[&capture],
    ));
    let mut lineage = claim(
        &root,
        "fork-lineage",
        agent_object(&root, "unused"),
        &[&capture],
    );
    lineage.fact = Fact::Relation {
        relation: Relation::ContextLineage {
            recipient: fork,
            checkpoint,
            original_owner: OwnerRef::Agent {
                agent: root.clone(),
            },
        },
    };
    claims.push(lineage);
    for (owner, active) in [(&root, false), (&grandchild, true)] {
        let activation =
            ActivationRef::native(owner.clone(), "execution", vec!["one".into()]).unwrap();
        claims.push(claim(
            owner,
            "activation",
            TraceObject::Activation(Activation {
                activation_ref: activation.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
            }),
            &[&capture],
        ));
        let mut boundary = claim(
            owner,
            "boundary",
            agent_object(owner, "unused"),
            &[&capture],
        );
        boundary.fact = Fact::Execution {
            execution: ExecutionRef::Activation(activation.clone()),
            boundary: if active {
                ExecutionBoundary::Started { time: None }
            } else {
                ExecutionBoundary::Outcome {
                    outcome: TerminalOutcome::Succeeded,
                    reason: None,
                    detail: None,
                }
            },
        };
        claims.push(boundary);
        if active {
            let mut coverage = claim(
                owner,
                "following",
                agent_object(owner, "unused"),
                &[&capture],
            );
            coverage.fact = Fact::Coverage {
                coverage: Coverage {
                    subject: AnyObjectRef::Activation(activation),
                    source: capture.source_position.source.clone(),
                    dimension: CoverageDimension::ExecutionBoundaries,
                    interval: CoverageInterval::Source {
                        first: capture.source_position.clone(),
                        through: capture.source_position.clone(),
                    },
                    state: CoverageState::Following,
                },
            };
            claims.push(coverage);
        }
    }
    fixture.publish(claims, &[&capture]).await;
    let boot = fixture.bootstrap(&root).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let (status, body) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}/relations?at_snapshot={token}",
            root.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: ReadPage<AgentDirectoryRow> = serde_json::from_value(body).unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|row| row.agent.address.agent())
            .collect::<Vec<_>>(),
        vec![root.clone(), child.clone(), grandchild.clone()]
    );
    assert_eq!(page.total.value, Some(3));
    for (filter, expected) in [
        ("completed", root.clone()),
        ("active", grandchild.clone()),
        ("unknown", child.clone()),
    ] {
        let (status, filtered) = fixture
            .get(&format!(
                "/api/v1/trace/agents/{}/relations?at_snapshot={token}&filter={filter}",
                root.encode()
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{filtered}");
        let page: ReadPage<AgentDirectoryRow> = serde_json::from_value(filtered).unwrap();
        assert_eq!(page.total.value, Some(1));
        assert_eq!(page.items[0].agent.address.agent(), expected);
    }

    assert_eq!(boot["header"]["descendantCount"]["value"], 2);
    let (status, body) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}/relations?at_snapshot={token}&q=grandchild",
            root.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: ReadPage<AgentDirectoryRow> = serde_json::from_value(body).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].agent.address.agent(), grandchild);
    let (status, body) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}/relations?at_snapshot={token}",
            child.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: ReadPage<AgentDirectoryRow> = serde_json::from_value(body).unwrap();
    assert_eq!(page.total.value, Some(2));
}

#[tokio::test]
async fn directory_membership_checks_spawn_proofs_without_borrowing_hidden_ancestor_metadata() {
    for hidden_link in [false, true] {
        let mut fixture = ProvenanceFixture::new().await;
        let root = agent("recipient", "root");
        let middle = agent("origin", "private-middle");
        let leaf = agent("recipient", "visible-leaf");
        let local = fixture
            .capture(&root, "local", b"root-owned topology")
            .await;
        let hidden = fixture
            .capture(&middle, "hidden", b"private metadata and edge")
            .await;
        let mut values = vec![
            claim(&root, "identity", agent_object(&root, "Root"), &[&local]),
            claim(
                &middle,
                "identity",
                agent_object(&middle, "Private middle name"),
                &[&hidden],
            ),
            claim(
                &leaf,
                "identity",
                agent_object(&leaf, "Visible leaf"),
                &[&local],
            ),
        ];
        for (index, parent, child) in [(0, &root, &middle), (1, &middle, &leaf)] {
            let mut edge = claim(
                &root,
                &format!("edge-{index}"),
                agent_object(&root, "unused"),
                &[if index == 0 && hidden_link {
                    &hidden
                } else {
                    &local
                }],
            );
            edge.fact = Fact::Relation {
                relation: Relation::Spawn {
                    parent: parent.clone(),
                    child: child.clone(),
                    operation: None,
                },
            };
            values.push(edge);
        }
        fixture.publish(values, &[&local, &hidden]).await;
        fixture
            .state
            .store
            .revoke_grant(&fixture.subject, "origin", "observe")
            .await
            .unwrap();
        let boot = fixture.bootstrap(&root).await;
        let token = boot["view"]["token"].as_str().unwrap();
        let (status, body) = fixture
            .get(&format!(
                "/api/v1/trace/agents/{}/relations?at_snapshot={token}",
                root.encode()
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.to_string().contains("Private middle name"));
        assert!(!body.to_string().contains(&middle.encode()));
        let page: ReadPage<AgentDirectoryRow> = serde_json::from_value(body).unwrap();
        let expected = if hidden_link {
            vec![root]
        } else {
            vec![root, leaf]
        };
        assert_eq!(
            page.items
                .into_iter()
                .map(|row| row.agent.address.agent())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(page.total.value, Some(expected.len() as u64));
    }
}

#[tokio::test]
async fn long_directory_navigation_rotates_scope_capacity_without_changing_the_snapshot_or_releasing_the_reader()
 {
    let mut fixture = ProvenanceFixture::new().await;
    let root = agent("recipient", "large-directory");
    let source = fixture
        .capture(&root, "root", b"retained parent topology")
        .await;
    fixture
        .publish(
            vec![claim(
                &root,
                "identity",
                agent_object(&root, "Root"),
                &[&source],
            )],
            &[&source],
        )
        .await;
    let count = fixture.state.trace.catalogue().limits().max_lease_scopes + 5;
    let mut values = vec![];
    let mut captures = vec![];
    for index in 0..count {
        let child = agent("recipient", &format!("child-{index:04}"));
        let capture = fixture
            .capture(
                &child,
                &format!("child-{index:04}"),
                b"separately retained child identity",
            )
            .await;
        values.push(claim(
            &child,
            "identity",
            agent_object(&child, &format!("Child {index}")),
            &[&capture],
        ));
        let mut edge = claim(
            &root,
            &format!("edge-{index:04}"),
            agent_object(&root, "unused"),
            &[&source],
        );
        edge.fact = Fact::Relation {
            relation: Relation::Spawn {
                parent: root.clone(),
                child,
                operation: None,
            },
        };
        values.push(edge);
        captures.push(capture);
        if values.len() >= 32 || index + 1 == count {
            let mut evidence: Vec<_> = captures.iter().collect();
            evidence.push(&source);
            fixture
                .publish(std::mem::take(&mut values), &evidence)
                .await;
            captures.clear();
        }
    }
    let boot = fixture.bootstrap(&root).await;
    let original = boot["view"].clone();
    let mut view = original.clone();
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    loop {
        let mut url = format!(
            "/api/v1/trace/agents/{}/relations?at_snapshot={}&limit=40",
            root.encode(),
            view["token"].as_str().unwrap()
        );
        if let Some(cursor) = cursor {
            url.push_str(&format!("&after={cursor}"));
        }
        let (status, body) = fixture.get(&url).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for row in body["items"].as_array().unwrap() {
            assert!(seen.insert(row["agent"]["address"].as_str().unwrap().to_owned()));
        }
        view = body["view"].clone();
        for field in ["generation", "evidenceCut", "viewRevision"] {
            assert_eq!(view[field], original[field]);
        }
        if !body["hasAfter"].as_bool().unwrap() {
            assert_eq!(body["total"]["value"], count + 1);
            break;
        }
        cursor = Some(body["afterCursor"].as_str().unwrap().to_owned());
    }
    assert_eq!(seen.len(), count + 1);
    assert_ne!(
        view["token"], original["token"],
        "scope pressure was not exercised"
    );
    fixture
        .state
        .trace
        .catalogue()
        .resolve_lease(
            &fixture.subject,
            original["token"].as_str().unwrap(),
            crate::models::now_ms(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_deleted_ancestry_proof_invalidates_checked_prefix_cursors_and_hides_its_subtree() {
    let mut fixture = ProvenanceFixture::new().await;
    let root = agent("recipient", "prefix-root");
    let middle = agent("recipient", "prefix-middle");
    let leaf = agent("recipient", "prefix-leaf");
    let witness = agent("origin", "ancestry-proof");
    let local = fixture.capture(&root, "local", b"visible identities").await;
    let proof = fixture
        .capture(&witness, "edge-proof", b"retained root to middle edge")
        .await;
    let mut claims: Vec<_> = [&root, &middle, &leaf]
        .into_iter()
        .map(|owner| claim(owner, "identity", agent_object(owner, "Visible"), &[&local]))
        .collect();
    for (index, parent, child, support) in
        [(0, &root, &middle, &proof), (1, &middle, &leaf, &local)]
    {
        let mut edge = claim(
            &root,
            &format!("edge-{index}"),
            agent_object(&root, "unused"),
            &[support],
        );
        edge.fact = Fact::Relation {
            relation: Relation::Spawn {
                parent: parent.clone(),
                child: child.clone(),
                operation: None,
            },
        };
        claims.push(edge);
    }
    fixture.publish(claims, &[&local, &proof]).await;
    let boot = fixture.bootstrap(&root).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let url = format!(
        "/api/v1/trace/agents/{}/relations?at_snapshot={token}&limit=2",
        root.encode()
    );
    let (status, first) = fixture.get(&url).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let cursor = first["afterCursor"].as_str().unwrap();
    fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(witness))
        .await
        .unwrap();
    let (status, rejected) = fixture.get(&format!("{url}&after={cursor}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    let (status, fresh) = fixture.get(&url).await;
    assert_eq!(status, StatusCode::OK, "{fresh}");
    assert_eq!(fresh["items"].as_array().unwrap().len(), 1);
    assert_eq!(fresh["items"][0]["agent"]["address"], root.encode());
    assert_eq!(fresh["total"]["value"], 1);
    assert!(!fresh.to_string().contains(&leaf.encode()));
    // Promoting the surviving leaf to a main reader starts a different subtree;
    // it does not borrow the now-unavailable path from the former root.
    let own = fixture.bootstrap(&leaf).await;
    assert_eq!(own["header"]["descendantCount"]["value"], 0);
}

#[tokio::test]
async fn corrected_spawn_intervals_publish_atomically_and_keep_old_directory_views_unchanged() {
    let mut fixture = ProvenanceFixture::new().await;
    let root = agent("recipient", "tree-root");
    let left = agent("recipient", "tree-left");
    let right = agent("recipient", "tree-right");
    let leaf = agent("recipient", "tree-leaf");
    let capture = fixture
        .capture(&root, "topology", b"initial retained tree")
        .await;
    let mut claims: Vec<_> = [&root, &left, &right, &leaf]
        .into_iter()
        .map(|owner| claim(owner, "identity", agent_object(owner, "Agent"), &[&capture]))
        .collect();
    let mut moved = None;
    for (index, parent, child) in [(0, &root, &left), (1, &root, &right), (2, &left, &leaf)] {
        let mut edge = claim(
            &root,
            &format!("edge-{index}"),
            agent_object(&root, "unused"),
            &[&capture],
        );
        edge.fact = Fact::Relation {
            relation: Relation::Spawn {
                parent: parent.clone(),
                child: child.clone(),
                operation: None,
            },
        };
        if index == 2 {
            moved = Some(edge.claim_ref.clone());
        }
        claims.push(edge);
    }
    fixture.publish(claims, &[&capture]).await;
    let before = fixture.bootstrap(&root).await;
    let old = before["view"]["token"].as_str().unwrap();
    let proof = fixture
        .capture(&root, "correction", b"explicit retained parent correction")
        .await;
    let mut correction = claim(
        &root,
        "corrected-edge",
        agent_object(&root, "unused"),
        &[&proof],
    );
    correction.fact = Fact::Relation {
        relation: Relation::Spawn {
            parent: right.clone(),
            child: leaf.clone(),
            operation: None,
        },
    };
    correction.supersedes.push(moved.unwrap());
    fixture.publish(vec![correction], &[&proof]).await;
    let after = fixture.bootstrap(&root).await;
    let current = after["view"]["token"].as_str().unwrap();
    for (token, parent, expected) in [
        (old, &left, vec![left.clone(), leaf.clone()]),
        (current, &left, vec![left.clone()]),
        (current, &right, vec![right.clone(), leaf.clone()]),
    ] {
        let (status, body) = fixture
            .get(&format!(
                "/api/v1/trace/agents/{}/relations?at_snapshot={token}",
                parent.encode()
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let page: ReadPage<AgentDirectoryRow> = serde_json::from_value(body).unwrap();
        assert_eq!(
            page.items
                .into_iter()
                .map(|row| row.agent.address.agent())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(page.total.value, Some(expected.len() as u64));
    }
}

#[tokio::test]
async fn large_filtered_counts_resume_without_hydrating_directory_bodies_or_lending_their_cursor_to_pages()
 {
    use sqlx::Connection;
    let mut fixture = ProvenanceFixture::new().await;
    let root = agent("recipient", "count-root");
    let source = fixture
        .capture(&root, "count-topology", b"retained catalogue")
        .await;
    fixture
        .publish(
            vec![claim(
                &root,
                "identity",
                agent_object(&root, "Root"),
                &[&source],
            )],
            &[&source],
        )
        .await;
    let count = fixture.generation.limits().max_candidates + 17;
    let mut values = vec![];
    for index in 0..count {
        let child = agent("recipient", &format!("count-child-{index:04}"));
        values.push(claim(
            &child,
            "identity",
            agent_object(&child, if index % 3 == 0 { "Needle" } else { "Other" }),
            &[&source],
        ));
        let mut edge = claim(
            &root,
            &format!("edge-{index:04}"),
            agent_object(&root, "unused"),
            &[&source],
        );
        edge.fact = Fact::Relation {
            relation: Relation::Spawn {
                parent: root.clone(),
                child,
                operation: None,
            },
        };
        values.push(edge);
        if values.len() >= 32 || index + 1 == count {
            fixture
                .publish(std::mem::take(&mut values), &[&source])
                .await;
        }
    }
    let boot = fixture.bootstrap(&root).await;
    assert_eq!(boot["header"]["descendantCount"]["complete"], false);
    let token = boot["view"]["token"].as_str().unwrap();
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(fixture.generation.path()),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE read_rows SET body_json='corrupt body' WHERE directory_end IS NOT NULL")
        .execute(&mut connection)
        .await
        .unwrap();
    let base = format!(
        "/api/v1/trace/agents/{}/relations?at_snapshot={token}&q=needle&count_only=true",
        root.encode()
    );
    let (status, first) = fixture.get(&base).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert!(first["items"].as_array().unwrap().is_empty());
    assert_eq!(first["state"], "work_budget_exhausted");
    assert_eq!(first["total"]["complete"], false);
    let cursor = first["afterCursor"].as_str().unwrap();
    let (status, rejected) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}/relations?at_snapshot={token}&q=needle&after={cursor}",
            root.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    let (status, end) = fixture.get(&format!("{base}&after={cursor}")).await;
    assert_eq!(status, StatusCode::OK, "{end}");
    assert!(end["items"].as_array().unwrap().is_empty());
    assert_eq!(end["hasAfter"], false);
    assert_eq!(end["total"]["value"], count.div_ceil(3));
    assert_eq!(end["total"]["complete"], true);
}

#[tokio::test]
async fn a_membership_grant_changed_after_scanning_prevents_a_directory_response() {
    let mut fixture = ProvenanceFixture::new().await;
    let root = agent("recipient", "racing-directory");
    let child = agent("recipient", "racing-child");
    let witness = agent("origin", "membership");
    let local = fixture.capture(&root, "local", b"local identities").await;
    let remote = fixture
        .capture(&witness, "membership", b"separate spawn proof")
        .await;
    let mut edge = claim(&root, "spawn", agent_object(&root, "unused"), &[&remote]);
    edge.fact = Fact::Relation {
        relation: Relation::Spawn {
            parent: root.clone(),
            child: child.clone(),
            operation: None,
        },
    };
    fixture
        .publish(
            vec![
                claim(&root, "root", agent_object(&root, "Root"), &[&local]),
                claim(&child, "child", agent_object(&child, "Child"), &[&local]),
                edge,
            ],
            &[&local, &remote],
        )
        .await;
    let boot = fixture.bootstrap(&root).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let url = format!(
        "/api/v1/trace/agents/{}/relations?at_snapshot={token}",
        root.encode()
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *fixture.state.trace.directory_read_pause.lock().await =
        Some((entered.clone(), release.clone()));
    let app = fixture.app.clone();
    let cookie = fixture.cookie.clone();
    let request = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .uri(url)
                .header(header::HOST, "localhost")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    fixture
        .state
        .store
        .revoke_grant(&fixture.subject, "origin", "observe")
        .await
        .unwrap();
    release.notify_one();
    let (status, body) = response(request.await.unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_cursor");
}
