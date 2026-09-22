use super::*;
use serde_json::value::RawValue;

const THREAD: &str = "01a0caaa-6425-7ea1-8100-a92dcc6a2869";
const TEXT: &str = "SYLPH_IDENTICAL_SUBMITTED_INPUT";
const NATIVE: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/codex/input/native.jsonl");
const WIRE: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/codex/input/wire.jsonl");
fn owner() -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:codex:{THREAD}")).unwrap())
}
fn delivery(index: usize) -> DeliveryRef {
    DeliveryRef::native(
        owner(),
        "sylph.command.delivery",
        vec![format!("fixture-command-{index}"), "submitted-v1".into()],
    )
    .unwrap()
}
fn auth() -> AuthorizationReceipt {
    AuthorizationReceipt {
        subject: "fixture-person".into(),
        decision_ref: "fixture-grant".into(),
        lease_ref: None,
        evaluated_at_ms: 1,
    }
}
struct Record {
    source: &'static str,
    index: u64,
    offset: u64,
    kind: TransportKind,
    raw: Vec<u8>,
}
#[derive(Deserialize)]
struct Wire<'a> {
    direction: String,
    #[serde(borrow)]
    frame: &'a RawValue,
}
fn records(fixture: &Fixture, order: SourceOrder, native: bool, live: bool) -> Vec<Record> {
    let mut controls = vec![];
    for index in 1..=2 {
        let command_id = format!("fixture-command-{index}");
        let accepted = OperationalFact::Accepted {
            command: Box::new(CommandAcceptance {
                command_id: command_id.clone(),
                target: CommandTarget::Agent { agent: owner() },
                kind: CommandKind::Send,
                input: SubmittedRevision {
                    revision: "submitted-v1".into(),
                    media_type: "text/plain".into(),
                    bytes: TEXT.as_bytes().to_vec(),
                },
                authorization: auth(),
                ingestion_authorization_epoch: fixture.epoch.clone(),
                retry_policy: RetryPolicy::AtMostOnce,
                idempotency_key: None,
                accepted_at_ms: 1,
            }),
        };
        for fact in [
            accepted,
            OperationalFact::Dispatched {
                command_id,
                input_revision: "submitted-v1".into(),
                attempt: 1,
                authorization: auth(),
            },
        ] {
            controls.push(Record {
                source: "controls",
                index: controls.len() as u64,
                offset: 0,
                kind: TransportKind::Control,
                raw: serde_json::to_vec(&fact).unwrap(),
            });
        }
    }
    let mut offset = 0;
    let native_records = if native {
        NATIVE
            .split_inclusive(|byte| *byte == b'\n')
            .enumerate()
            .map(|(index, raw)| {
                let record = Record {
                    source: "native",
                    index: index as u64,
                    offset,
                    kind: TransportKind::NativeJsonl,
                    raw: raw.to_vec(),
                };
                offset += raw.len() as u64;
                record
            })
            .collect()
    } else {
        vec![]
    };
    let live_records = if live {
        WIRE.split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .enumerate()
            .map(|(index, line)| {
                let wire: Wire<'_> = serde_json::from_slice(line).unwrap();
                Record {
                    source: "wire",
                    index: index as u64,
                    offset: 0,
                    kind: if wire.direction == "out" {
                        TransportKind::LiveStdin
                    } else {
                        TransportKind::LiveStdout
                    },
                    raw: wire.frame.get().as_bytes().to_vec(),
                }
            })
            .collect()
    } else {
        vec![]
    };
    let mut groups = vec![controls, native_records, live_records];
    match order {
        SourceOrder::NativeFirst => groups.into_iter().flatten().collect(),
        SourceOrder::LiveFirst => {
            groups.reverse();
            groups.into_iter().flatten().collect()
        }
        SourceOrder::Alternating => {
            let mut groups: Vec<_> = groups.into_iter().map(Vec::into_iter).collect();
            let mut all = vec![];
            loop {
                let next: Vec<_> = groups.iter_mut().filter_map(Iterator::next).collect();
                if next.is_empty() {
                    break;
                }
                all.extend(next);
            }
            all
        }
    }
}
async fn admit(fixture: &Fixture, record: &Record) -> Capture {
    let issuer = if record.kind == TransportKind::Control {
        Issuer::Server("input-controller".into())
    } else {
        Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap())
    };
    let capture = Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: record.source.into(),
            producer_epoch: "retained".into(),
            producer_seq: record.index + 1,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer: issuer.clone(),
                instance: record.source.into(),
            },
            generation: 0,
            locator: if record.kind == TransportKind::NativeJsonl {
                SourceLocator::Jsonl {
                    byte_start: record.offset,
                    byte_end: record.offset + record.raw.len() as u64,
                    record_ordinal: record.index,
                }
            } else {
                SourceLocator::Frame {
                    ordinal: record.index,
                }
            },
        },
        ingestion_authorization_epoch: fixture.epoch.clone(),
        declared_subject_refs: vec![owner()],
        captured_at: TimeEvidence {
            domain: "fixture-receipt".into(),
            value: 1,
            unit: TimeUnit::Milliseconds,
            precision: None,
            basis: TimeBasis::ServerReceipt,
        },
        source_clock: None,
        transport_kind: record.kind,
        vendor_build: Some("retained-0.155.1".into()),
        schema_fingerprint: None,
        payload_ref: fixture.stage(&record.raw).await,
        attachments: vec![],
        capture_diagnostics: vec![],
    };
    fixture
        .evidence
        .admit_batch(&issuer, std::slice::from_ref(&capture))
        .await
        .unwrap();
    capture
}
async fn expected(fixture: &Fixture, store: &GenerationStore) {
    let reduced = reduction(&claims(store).await);
    for index in 1..=2 {
        let reference = delivery(index);
        assert!(
            reduced.delivery_stages[&reference]
                .iter()
                .any(|(stage, _)| matches!(stage, DeliveryStage::Admitted { entry: Some(_) })),
            "the recorded client ID must establish this exact request's admission"
        );
        assert!(
            !reduced.delivery_stages[&reference]
                .iter()
                .any(|(stage, _)| matches!(stage, DeliveryStage::Included { .. }))
        );
        let binding = reduced.select_delivery_revision(&reference, None);
        let original = MessageRevisionRef::native(
            owner(),
            "sylph.command.revision",
            vec![format!("fixture-command-{index}"), "submitted-v1".into()],
        )
        .unwrap();
        assert_eq!(binding.known(), Some(&original));
        let message = MessageRef::native(
            owner(),
            "sylph.command.message",
            vec![format!("fixture-command-{index}")],
        )
        .unwrap();
        assert_eq!(
            revision_bytes(fixture, store, &message).await,
            vec![TEXT.as_bytes().to_vec()]
        );
    }
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let page = store
        .read_page(
            &snapshot,
            &read_scope(&owner(), None, "history"),
            None,
            None,
            None,
            64,
            false,
        )
        .await
        .unwrap();
    let units: Vec<ReadingUnit> = page
        .rows
        .into_iter()
        .map(|row| serde_json::from_value(row.body).unwrap())
        .collect();
    let input_units: Vec<_> = units
        .iter()
        .filter_map(|unit| match &unit.content {
            ReadingContent::Message {
                parts, delivery, ..
            } if parts.iter().any(|part| part.preview.contains(TEXT)) => Some((unit, delivery)),
            _ => None,
        })
        .collect();
    assert_eq!(
        input_units.len(),
        2,
        "native and display presentations cannot duplicate the two real submissions"
    );
    let delivered: BTreeSet<_> = input_units
        .iter()
        .map(|(_, delivery)| delivery.as_ref().unwrap().reference.clone())
        .collect();
    assert_eq!(
        delivered,
        [
            TraceAddress::of(&AnyObjectRef::Delivery(delivery(1))),
            TraceAddress::of(&AnyObjectRef::Delivery(delivery(2)))
        ]
        .into_iter()
        .collect()
    );
    for (unit, _) in input_units {
        if let ReadingContent::Message {
            revision_ref,
            parts,
            ..
        } = &unit.content
        {
            assert!(
                matches!(revision_ref.address(),ObjectAddress::Native{namespace,..}if namespace=="sylph.command.revision")
            );
            assert_eq!(parts[0].preview, TEXT);
        }
    }
}
async fn replay(order: SourceOrder, incremental: bool, native: bool, live: bool) -> Projection {
    let mut fixture = Fixture::new().await;
    fixture.scope = IngestionScope::Agent(owner());
    fixture.epoch = fixture
        .evidence
        .authorize_scope(&fixture.scope, &[owner()])
        .await
        .unwrap();
    let (mut store, _) = fixture.generation("input").await;
    let mut captures = vec![];
    for record in records(&fixture, order, native, live) {
        let capture = admit(&fixture, &record).await;
        fixture
            .evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        captures.push(capture);
        if incremental {
            fixture
                .worker_with(WorkerLimits {
                    max_unit_observations: 1 + record.index as usize % 3,
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            if record.index % 4 == 0 {
                store = reopened(store).await;
            }
        }
    }
    for mut capture in captures {
        capture.observation_ref.producer_id.push_str("-reread");
        fixture
            .evidence
            .admit_batch(&capture.observation_ref.issuer.clone(), &[capture])
            .await
            .unwrap();
    }
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    expected(&fixture, &store).await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("input-rebuild").await;
    fixture.worker().catch_up(&fresh, 1, cut).await.unwrap();
    expected(&fixture, &fresh).await;
    assert_projection(&projection(&fixture, &fresh).await, &before);
    Projection {
        claims: before.claims,
        reader: before
            .reader
            .normalize_separate_archive_epochs(&fixture.catalogue)
            .await,
    }
}
#[tokio::test]
async fn recorded_client_input_ids_keep_equal_submissions_distinct_through_native_live_and_restart_replay()
 {
    let cold = replay(SourceOrder::NativeFirst, false, true, true).await;
    assert_projection(
        &replay(SourceOrder::LiveFirst, true, true, true).await,
        &cold,
    );
    assert_projection(
        &replay(SourceOrder::Alternating, true, true, true).await,
        &cold,
    );
    replay(SourceOrder::NativeFirst, true, true, false).await;
    replay(SourceOrder::NativeFirst, true, false, true).await;
}

async fn fixture() -> Fixture {
    let mut fixture = Fixture::new().await;
    fixture.scope = IngestionScope::Agent(owner());
    fixture.epoch = fixture
        .evidence
        .authorize_scope(&fixture.scope, &[owner()])
        .await
        .unwrap();
    fixture
}
#[tokio::test]
async fn transformed_vendor_input_preserves_submitted_bytes_and_unattempted_input_cannot_gain_admission()
 {
    for dispatched in [false, true] {
        let fixture = fixture().await;
        let (store, _) = fixture.generation("transformed-input").await;
        let mut offsets = BTreeMap::<String, u64>::new();
        let mut sequences = BTreeMap::<String, u64>::new();
        for mut record in records(&fixture, SourceOrder::LiveFirst, true, true) {
            let mut value: serde_json::Value = serde_json::from_slice(&record.raw).unwrap();
            if !dispatched && value["kind"] == "dispatched" {
                continue;
            }
            // This is an explicit synthetic transformation of the retained pair.
            // The fixture's actual vendor identifiers and correlation fields stay.
            let payload_key = if record.kind == TransportKind::NativeJsonl {
                "payload"
            } else {
                "params"
            };
            if let Some(payload) = value.get_mut(payload_key) {
                if payload["type"] == "message"
                    && payload["role"] == "user"
                    && payload["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                        == serde_json::json!(["user.text"])
                {
                    payload["content"][0]["text"] = format!("VENDOR_WRAPPER\n{TEXT}").into();
                }
                if matches!(
                    payload["item"]["type"].as_str(),
                    Some("userMessage" | "UserMessage")
                ) {
                    payload["item"]["content"][0]["text"] =
                        format!("VENDOR_WRAPPER\n{TEXT}").into();
                }
            }
            record.raw = serde_json::to_vec(&value).unwrap();
            if record.kind == TransportKind::NativeJsonl {
                record.raw.push(b'\n');
            }
            let offset = offsets.entry(record.source.into()).or_default();
            record.offset = *offset;
            *offset += record.raw.len() as u64;
            let sequence = sequences.entry(record.source.into()).or_default();
            record.index = *sequence;
            *sequence += 1;
            admit(&fixture, &record).await;
        }
        fixture
            .worker()
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        let reduced = reduction(&claims(&store).await);
        if dispatched {
            expected(&fixture, &store).await;
            let observed = MessageRef::native(
                owner(),
                "codex.message",
                vec!["01a0caaa-64d4-7d63-aaf5-cf6645660777".into()],
            )
            .unwrap();
            let bodies = revision_bytes(&fixture, &store, &observed).await;
            assert!(!bodies.is_empty());
            assert!(
                bodies
                    .iter()
                    .all(|body| body == format!("VENDOR_WRAPPER\n{TEXT}").as_bytes())
            );
        } else {
            for index in 1..=2 {
                assert!(
                    reduced.delivery_stages[&delivery(index)]
                        .iter()
                        .all(|(stage, _)| *stage == DeliveryStage::Requested)
                );
            }
            assert!(!reduced.relations.iter().any(|relation| matches!(&relation.relation, Relation::Representation { original, .. } if matches!(original.address(), ObjectAddress::Native { namespace, .. } if namespace == "sylph.command.revision"))));
        }
    }
}

#[tokio::test]
async fn admitted_input_previews_require_both_submission_and_vendor_retention_epochs() {
    let mut fixture = Fixture::new().await;
    let vendor_epoch = fixture
        .evidence
        .authorize_scope(&fixture.scope, &[owner()])
        .await
        .unwrap();
    let control_scope = IngestionScope::Agent(owner());
    let control_epoch = fixture
        .evidence
        .authorize_scope(&control_scope, &[owner()])
        .await
        .unwrap();
    fixture.epoch = control_epoch.clone();
    let records = records(&fixture, SourceOrder::NativeFirst, true, true);
    let (store, _) = fixture.generation("input-provenance").await;
    for record in records {
        fixture.epoch = if record.kind == TransportKind::Control {
            control_epoch.clone()
        } else {
            vendor_epoch.clone()
        };
        admit(&fixture, &record).await;
    }
    fixture
        .worker()
        .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let rows = store
        .read_page(
            &snapshot,
            &read_scope(&owner(), None, "history"),
            None,
            None,
            None,
            64,
            false,
        )
        .await
        .unwrap();
    let mut inputs = 0;
    for row in rows.rows {
        let unit: ReadingUnit = serde_json::from_value(row.body).unwrap();
        if matches!(&unit.content, ReadingContent::Message { parts, .. } if parts.iter().any(|part| part.preview == TEXT))
        {
            inputs += 1;
            assert!(
                row.epoch == control_epoch || row.dependency_epochs.contains(&control_epoch),
                "the preview contains control-owned bytes, so vendor retention alone cannot authorize it"
            );
            assert!(
                row.epoch == vendor_epoch || row.dependency_epochs.contains(&vendor_epoch),
                "the admission and history position require their vendor proof"
            );
        }
    }
    assert_eq!(inputs, 2);
}
