use super::*;
use crate::trace::reading_store::read_scope;
use serde_json::json;

#[tokio::test]
async fn independent_admissions_of_one_source_keep_one_message_and_survive_origin_deletion() {
    let fixture = Fixture::new().await;
    let lines: Vec<_> = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .collect();
    let header = fixture.native(1, 0, lines[0]).await;
    let answer = fixture.native(2, lines[0].len() as u64, lines[3]).await;
    let second_scope = IngestionScope::Inventory(InventoryRef {
        environment: agent().environment().clone(),
        vendor: AgentName::parse("codex").unwrap(),
        instance: "independent-retained-copy".into(),
    });
    let second_epoch = fixture
        .evidence
        .authorize_scope(&second_scope, &[])
        .await
        .unwrap();
    for (index, original) in [header.clone(), answer.clone()].into_iter().enumerate() {
        let mut copy = original;
        copy.observation_ref.producer_seq = index as u64 + 3;
        copy.ingestion_authorization_epoch = second_epoch.clone();
        fixture
            .evidence
            .admit_batch(&copy.observation_ref.issuer, std::slice::from_ref(&copy))
            .await
            .unwrap();
    }
    for (sequence, epoch, proof_sequence) in
        [(5, fixture.epoch.clone(), 1), (6, second_epoch.clone(), 3)]
    {
        let definition = SourceDefinition {
            source: header.source_position.source.clone(),
            generation: 0,
            locator: "/native/rollout.jsonl".into(),
            owner: agent(),
            basis: SourceOwnerBasis::NativeIdentity {
                observation: native_observation(proof_sequence),
            },
        };
        let mut capture = header.clone();
        capture.observation_ref.producer_seq = sequence;
        capture.ingestion_authorization_epoch = epoch;
        capture.transport_kind = TransportKind::SourceInventory;
        capture.source_position = SourcePosition {
            source: SourceRef {
                issuer: capture.observation_ref.issuer.clone(),
                instance: format!("inventory-{sequence}"),
            },
            generation: 0,
            locator: SourceLocator::Frame { ordinal: 0 },
        };
        capture.payload_ref = fixture
            .stage(&serde_json::to_vec(&definition).unwrap())
            .await;
        fixture
            .evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
    }
    for deleted in [false, true] {
        if deleted {
            fixture.evidence.delete_scope(&fixture.scope).await.unwrap();
        }
        let (generation, _) = fixture
            .generation(if deleted {
                "retained-copy"
            } else {
                "two-copies"
            })
            .await;
        fixture.worker().catch_up(&generation, 1, 6).await.unwrap();
        let snapshot = generation.checkpoint().await.unwrap().snapshot;
        let page = generation
            .read_page(
                &snapshot,
                &read_scope(&agent(), None, "history"),
                None,
                None,
                None,
                8,
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            page.rows.len(),
            1,
            "independent retention cannot duplicate conversation content"
        );
        assert!(page.rows[0]
            .body
            .to_string()
            .contains("The transcript is available."));
        let revisions: BTreeSet<_> = claims(&generation)
            .await
            .into_iter()
            .filter_map(|claim| match claim.fact {
                Fact::Declare {
                    object: TraceObject::MessageRevision(revision),
                } if revision.message == message("answer-1") => Some(revision.revision_ref),
                _ => None,
            })
            .collect();
        assert_eq!(
            revisions.len(),
            1,
            "agreeing proof claims must retain one domain revision"
        );
        let bodies = revision_bytes(&fixture, &generation, &message("answer-1")).await;
        assert!(!bodies.is_empty());
        assert!(bodies
            .iter()
            .all(|body| body == b"The transcript is available."));
    }
}

#[tokio::test]
async fn unknown_required_vendor_kind_retains_exact_bytes_and_an_evidenced_decode_gap() {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let unknown = b" {\"type\":\"response_item\",\"payload\":{\"type\":\"future_required_kind\",\"id\":\"future\",\"field\":1,\"field\":2}} \r\n";
    let capture = fixture.native(2, header.len() as u64, unknown).await;
    let (generation, _) = fixture.generation("unknown-kind").await;
    fixture.worker().catch_up(&generation, 1, 2).await.unwrap();
    let retained = claims(&generation).await;
    assert!(
        retained.iter().any(|claim| matches!(&claim.fact,
            Fact::Diagnostic { diagnostic } if diagnostic.code == DiagnosticCode::UnsupportedKind
                && diagnostic.message.contains("future_required_kind")
                && claim.evidence.contains(&capture.observation_ref)
        )),
        "an unknown required discriminator must remain a sourced diagnostic"
    );
    assert!(
        !retained.iter().any(|claim| matches!(&claim.fact,
            Fact::HistoryMappingComplete { .. } if claim.evidence.contains(&capture.observation_ref)
        )),
        "unrecognized content cannot certify an empty, completely mapped history interval"
    );
    assert!(!retained.iter().any(|claim| matches!(
        claim.fact,
        Fact::Declare {
            object: TraceObject::Message(_) | TraceObject::Activation(_)
        }
    )));
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&capture.observation_ref, unknown.len())
            .await
            .unwrap(),
        Some(unknown.to_vec())
    );
    let (rebuilt, _) = fixture.generation("unknown-kind-rebuilt").await;
    fixture.worker().catch_up(&rebuilt, 1, 2).await.unwrap();
    assert_eq!(claims(&rebuilt).await, retained);
}

#[tokio::test]
async fn receipt_clocks_never_fill_missing_native_execution_endpoints() {
    for (start, end) in [
        (None, None),
        (Some(41), None),
        (None, Some(44)),
        (Some(41), Some(44)),
    ] {
        let fixture = Fixture::new().await;
        let header = CODEX_NATIVE
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .unwrap();
        let header_capture = fixture.native(1, 0, header).await;
        let mut offset = header.len() as u64;
        for (seq, kind, key, stamp) in [
            (2, "task_started", "started_at", start),
            (3, "task_complete", "completed_at", end),
        ] {
            let mut payload = json!({"type":kind,"turn_id":"native-clock"});
            if let Some(stamp) = stamp {
                payload[key] = json!(stamp);
            }
            let mut raw =
                serde_json::to_vec(&json!({"type":"event_msg","payload":payload})).unwrap();
            raw.push(b'\n');
            let mut capture = header_capture.clone();
            capture.observation_ref.producer_seq = seq;
            capture.source_position.locator = SourceLocator::Jsonl {
                byte_start: offset,
                byte_end: offset + raw.len() as u64,
                record_ordinal: seq - 1,
            };
            capture.payload_ref = fixture.stage(&raw).await;
            capture.captured_at = TimeEvidence {
                domain: "server-receipt-only".into(),
                value: (seq * 1000) as i64,
                unit: TimeUnit::Milliseconds,
                precision: Some(1),
                basis: TimeBasis::ServerReceipt,
            };
            fixture
                .evidence
                .admit_batch(
                    &capture.observation_ref.issuer,
                    std::slice::from_ref(&capture),
                )
                .await
                .unwrap();
            offset += raw.len() as u64;
        }
        let (generation, _) = fixture.generation("native-clocks").await;
        let worker = fixture.worker();
        worker.catch_up(&generation, 1, 3).await.unwrap();
        let (_, reduction, _) = worker.projected_snapshot(&generation).await.unwrap();
        let activation = AnyObjectRef::Activation(
            ActivationRef::native(agent(), "codex.turn", vec!["native-clock".into()]).unwrap(),
        );
        let execution = reduction.execution(&activation).unwrap();
        let starts: Vec<_> = execution
            .boundaries
            .iter()
            .filter_map(|(boundary, _)| match boundary {
                ExecutionBoundary::Started { time } => Some(time.clone()),
                _ => None,
            })
            .collect();
        let ends: Vec<_> = execution
            .boundaries
            .iter()
            .filter_map(|(boundary, _)| match boundary {
                ExecutionBoundary::Terminated { time, .. } => Some(time.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts
                .iter()
                .map(|time| time.as_ref().map(|time| time.value))
                .collect::<Vec<_>>(),
            vec![start]
        );
        assert_eq!(
            ends.iter()
                .map(|time| time.as_ref().map(|time| time.value))
                .collect::<Vec<_>>(),
            vec![end]
        );
        for time in starts.iter().chain(&ends).flatten() {
            assert_eq!(time.basis, TimeBasis::SourceWall);
            assert_eq!(time.unit, TimeUnit::Seconds);
            assert_eq!(time.domain, "codex-wall:env7fk2");
        }
        let snapshot = generation.checkpoint().await.unwrap().snapshot;
        let page = generation
            .read_page(
                &snapshot,
                &read_scope(&agent(), None, "activations"),
                None,
                None,
                None,
                8,
                false,
            )
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        let summary: ExecutionSummary = serde_json::from_value(page.rows[0].body.clone()).unwrap();
        if start.is_some() && end.is_some() {
            assert_eq!(summary.durations.len(), 1);
            assert_eq!(summary.durations[0].value.as_i64(), Some(3));
            assert_eq!(summary.durations[0].unit, TimeUnit::Seconds);
        } else {
            assert!(
                summary.durations.is_empty(),
                "missing endpoints cannot acquire a plausible duration from receipts"
            );
        }
    }
}

#[tokio::test]
async fn orphan_output_admission_survives_late_invocation_and_rebuild() {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let raw = b"{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"late-call\",\"output\":\"retained orphan output\"}}\n";
    let capture = fixture.native(2, header.len() as u64, raw).await;
    let (generation, _) = fixture.generation("orphan-output").await;
    fixture.worker().catch_up(&generation, 1, 2).await.unwrap();
    let retained = claims(&generation).await;
    assert!(!retained.iter().any(|claim| matches!(
        &claim.fact,
        Fact::Declare {
            object: TraceObject::Operation(_)
        } | Fact::Execution { .. }
    )));
    let result = retained
        .iter()
        .find_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Result(result),
            } => Some(result.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        result.delivery.is_some(),
        "recorded output proves admission even without the invocation"
    );
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&capture.observation_ref, raw.len())
            .await
            .unwrap(),
        Some(raw.to_vec())
    );
    for phase in 0..3 {
        let rebuilt;
        let current = if phase == 2 {
            rebuilt = fixture.generation("orphan-output-rebuilt").await.0;
            fixture.worker().catch_up(&rebuilt, 1, 4).await.unwrap();
            &rebuilt
        } else {
            &generation
        };
        let snapshot = current.checkpoint().await.unwrap().snapshot;
        let page = current
            .read_page(
                &snapshot,
                &read_scope(&agent(), None, "history"),
                None,
                None,
                None,
                8,
                false,
            )
            .await
            .unwrap();
        assert!(
            page.rows
                .iter()
                .any(|row| row.body.to_string().contains("retained orphan output")),
            "orphan content must remain readable in phase {phase}"
        );
        let observed = claims(current).await;
        assert!(
            observed.iter().any(|claim| matches!(&claim.fact,
                Fact::Declare { object: TraceObject::Result(found) } if found == &result
            )),
            "late invocation and rebuild must retain the same result"
        );
        if phase == 0 {
            fixture.native_on("late-invocation", 3, 0, header).await;
            fixture.native_on("late-invocation", 4, header.len() as u64,
                b"{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"late-call\",\"name\":\"exec\",\"arguments\":\"{}\"}}\n").await;
            fixture.worker().catch_up(&generation, 1, 4).await.unwrap();
        } else {
            assert!(observed.iter().any(|claim| matches!(&claim.fact,
                Fact::Declare { object: TraceObject::Operation(operation) } if operation.operation_ref == result.operation
            )));
        }
    }
}

#[tokio::test]
async fn orphan_claude_delegation_resolves_from_a_late_invocation_on_another_source() {
    let fixture = Fixture::for_vendor("claude").await;
    let returned = br#"{"type":"user","sessionId":"late-root","uuid":"result-a","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"delegate-a","content":"child answer"}]},"toolUseResult":{"agentId":"late-child","status":"completed"}}"#;
    let result_capture = fixture.native_on("result-source", 1, 0, returned).await;
    let (generation, _) = fixture.generation("late-claude-call").await;
    fixture.worker().catch_up(&generation, 1, 1).await.unwrap();
    assert!(!claims(&generation).await.iter().any(|claim| matches!(
        &claim.fact,
        Fact::Declare {
            object: TraceObject::Agent(AgentThread {
                agent_ref: AgentRef::Embedded { .. },
                ..
            })
        }
    )));
    let call_capture = fixture.native_on("call-source", 2, 0,
        br#"{"type":"assistant","sessionId":"late-root","uuid":"call-a","message":{"role":"assistant","content":[{"type":"tool_use","id":"delegate-a","name":"Agent","input":{"prompt":"perform work"}}]}}"#).await;
    fixture.worker().catch_up(&generation, 1, 2).await.unwrap();
    let root = AgentRef::Session(SessionId::decode("env7fk2:claude:late-root").unwrap());
    let child = AgentRef::Embedded {
        anchor_session: root.anchor_session().clone(),
        namespace: "claude.agent".into(),
        native_child_id: "late-child".into(),
    };
    for current in [
        &generation,
        &fixture.generation("late-claude-call-rebuilt").await.0,
    ] {
        fixture.worker().catch_up(current, 1, 2).await.unwrap();
        let retained = claims(current).await;
        let proof = retained
            .iter()
            .find(|claim| {
                matches!(&claim.fact,
                    Fact::Declare { object: TraceObject::Agent(agent) } if agent.agent_ref == child
                )
            })
            .expect("late retained invocation must resolve the pending child result");
        assert!(proof.evidence.contains(&result_capture.observation_ref));
        assert!(proof.evidence.contains(&call_capture.observation_ref));
    }
}
