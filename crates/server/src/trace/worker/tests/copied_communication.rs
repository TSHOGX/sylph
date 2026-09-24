//! Minimal copied-prefix wrappers around complete retained vendor payloads.
use super::*;
use crate::trace::reading_store::read_scope;
use serde_json::{Value, json};

const CHILD: &str = "01a083fa-c22e-7dc3-9cc3-2b5c95bda28b";
const ROOT: &str = "01a083c2-1389-77c2-a16c-302aa2bbf884";
fn participant(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:codex:{id}")).unwrap())
}
fn records() -> (Vec<Value>, Vec<Value>, String, Vec<Value>) {
    let retained: Vec<Value> =
        include_str!("../../../../../../fixtures/unified-trace/codex/reuse/child.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let index = retained
        .iter()
        .position(|record| record["payload"]["type"] == "agent_message")
        .unwrap();
    assert_eq!(
        retained[index - 1]["type"],
        "inter_agent_communication_metadata"
    );
    let original = vec![
        retained[0].clone(),
        retained[index - 1].clone(),
        retained[index].clone(),
    ];
    let id = retained[index]["payload"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let parts = retained[index]["payload"]["content"]
        .as_array()
        .unwrap()
        .clone();
    let mut metadata = retained[index - 1].clone();
    metadata["ordinal"] = 1.into();
    let mut message = retained[index].clone();
    message["ordinal"] = 2.into();
    let copied = vec![
        json!({"type":"session_meta","ordinal":0,"payload":{"id":"copy-reader","session_id":"copy-reader","agent_path":"/root","forked_from_id":CHILD,"subagent_history_start_ordinal":3}}),
        metadata,
        message,
    ];
    (original, copied, id, parts)
}
async fn capture_records(
    fixture: &Fixture,
    source: &str,
    epoch: &IngestionEpoch,
    records: &[Value],
) {
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    let mut offset = 0;
    for (index, value) in records.iter().enumerate() {
        let mut raw = serde_json::to_vec(value).unwrap();
        raw.push(b'\n');
        let capture = Capture {
            observation_ref: ObservationRef {
                issuer: issuer.clone(),
                producer_id: source.into(),
                producer_epoch: "retained".into(),
                producer_seq: index as u64 + 1,
            },
            source_position: SourcePosition {
                source: SourceRef {
                    issuer: issuer.clone(),
                    instance: source.into(),
                },
                generation: 0,
                locator: SourceLocator::Jsonl {
                    byte_start: offset,
                    byte_end: offset + raw.len() as u64,
                    record_ordinal: index as u64,
                },
            },
            ingestion_authorization_epoch: epoch.clone(),
            declared_subject_refs: if source == "copy" {
                vec![participant("copy-reader")]
            } else {
                vec![]
            },
            captured_at: TimeEvidence {
                domain: "fixture.wall".into(),
                value: 100,
                unit: TimeUnit::Milliseconds,
                precision: Some(1),
                basis: TimeBasis::SourceWall,
            },
            source_clock: None,
            transport_kind: TransportKind::NativeJsonl,
            vendor_build: None,
            schema_fingerprint: None,
            payload_ref: fixture.stage(&raw).await,
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        fixture
            .evidence
            .admit_batch(&issuer, std::slice::from_ref(&capture))
            .await
            .unwrap();
        offset += raw.len() as u64;
    }
}
async fn expected(
    fixture: &Fixture,
    store: &GenerationStore,
    id: &str,
    parts: &[Value],
    origin: bool,
) {
    let reference =
        MessageRef::native(participant(CHILD), "codex.message", vec![id.into()]).unwrap();
    let retained = claims(store).await;
    let mut reducer = sylph_trace::reducer::Reducer::default();
    reducer
        .apply(
            &retained,
            &retained
                .iter()
                .flat_map(|claim| claim.evidence.iter().cloned())
                .collect(),
            retained.len(),
        )
        .unwrap();
    let reduced = reducer.resolve();
    let Some(TraceObject::Message(message)) = reduced
        .object(&AnyObjectRef::Message(reference.clone()))
        .and_then(|value| value.declaration.known())
    else {
        panic!("copied message identity must remain readable")
    };
    assert_eq!(
        message.author,
        if origin {
            Author::Agent {
                agent: OwnerRef::Agent {
                    agent: participant(ROOT),
                },
            }
        } else {
            Author::Unknown
        }
    );
    assert_eq!(message.purpose, MessagePurpose::TaskInput);
    let mut revisions = 0;
    for claim in &retained {
        let Fact::Declare {
            object: TraceObject::MessageRevision(revision),
        } = &claim.fact
        else {
            continue;
        };
        if revision.message != reference {
            continue;
        }
        revisions += 1;
        assert_eq!(revision.parts.len(), parts.len());
        for (part, expected) in revision.parts.iter().zip(parts) {
            let bytes = fixture
                .evidence
                .payload_bytes(&part.payload, 1024 * 1024)
                .await
                .unwrap();
            if expected["type"] == "input_text" {
                assert_eq!(part.kind, ContentKind::Text);
                assert_eq!(bytes, expected["text"].as_str().unwrap().as_bytes());
            } else {
                assert_eq!(expected["type"], "encrypted_content");
                assert_eq!(part.kind, ContentKind::Opaque);
                assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), *expected);
            }
        }
    }
    assert!(revisions > 0, "the retained copy must expose every part");
    assert_eq!(
        reduced
            .objects
            .values()
            .filter(|value| matches!(value.declaration.known(), Some(TraceObject::Delivery(_))))
            .count(),
        usize::from(origin)
    );
    assert!(reduced.executions.is_empty());
    assert!(
        !reduced
            .relations
            .iter()
            .any(|value| matches!(value.relation, Relation::Trigger { .. }))
    );
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let page = store
        .read_page(
            &snapshot,
            &read_scope(&participant("copy-reader"), None, "history"),
            None,
            None,
            None,
            store.limits().max_items,
            false,
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    let unit: ReadingUnit = serde_json::from_value(page.rows[0].body.clone()).unwrap();
    assert!(matches!(unit.content, ReadingContent::Message { .. }));
    let Some(ReadEntryContext::Inherited {
        original_owner: Some(original),
    }) = unit.entry_context
    else {
        panic!("copied body lost its original history owner")
    };
    assert_eq!(original.address.agent(), participant(CHILD));
}
#[tokio::test]
async fn a_copied_communication_survives_missing_late_and_deleted_origin_without_replaying_delivery()
 {
    for original_first in [false, true] {
        let fixture = Fixture::new().await;
        let copy_scope = IngestionScope::Agent(participant("copy-reader"));
        let copy_epoch = fixture
            .evidence
            .authorize_scope(&copy_scope, &[participant("copy-reader")])
            .await
            .unwrap();
        let (original, copied, id, parts) = records();
        let (store, _) = fixture.generation("copied-communication").await;
        if original_first {
            capture_records(&fixture, "original", &fixture.epoch, &original).await;
        }
        capture_records(&fixture, "copy", &copy_epoch, &copied).await;
        fixture
            .worker()
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        expected(&fixture, &store, &id, &parts, original_first).await;
        if !original_first {
            capture_records(&fixture, "original", &fixture.epoch, &original).await;
            fixture
                .worker()
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            expected(&fixture, &store, &id, &parts, true).await;
        }
        let before = reader_projection(&fixture, &store).await;
        let (rebuilt, _) = fixture.generation("copied-rebuilt").await;
        fixture
            .worker()
            .catch_up(&rebuilt, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        expected(&fixture, &rebuilt, &id, &parts, true).await;
        assert_eq!(reader_projection(&fixture, &rebuilt).await, before);
        fixture.evidence.delete_scope(&fixture.scope).await.unwrap();
        let (remaining, _) = fixture.generation("copied-origin-deleted").await;
        fixture
            .worker()
            .catch_up(
                &remaining,
                1,
                fixture.evidence.evidence_cut().await.unwrap(),
            )
            .await
            .unwrap();
        expected(&fixture, &remaining, &id, &parts, false).await;
    }
}
