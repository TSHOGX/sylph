use super::*;

const THREAD: &str = "01a0caaa-6425-7ea1-8100-a92dcc6a2869";
fn records() -> Vec<Value> {
    include_str!("../../../../../../fixtures/unified-trace/codex/input/native.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn archive(target: &AgentRef) -> ArchiveContext {
    let mut archive = ArchiveContext::default();
    for index in 1..=2 {
        let token = format!("sylph:input:fixture-command-{index}");
        for (namespace, reference) in [
            (
                "sylph.dispatched-input.delivery",
                AnyObjectRef::Delivery(
                    DeliveryRef::native(
                        target.clone(),
                        "sylph.command.delivery",
                        vec![format!("fixture-command-{index}"), "submitted-v1".into()],
                    )
                    .unwrap(),
                ),
            ),
            (
                "sylph.dispatched-input.revision",
                AnyObjectRef::MessageRevision(
                    MessageRevisionRef::native(
                        target.clone(),
                        "sylph.command.revision",
                        vec![format!("fixture-command-{index}"), "submitted-v1".into()],
                    )
                    .unwrap(),
                ),
            ),
        ] {
            let key = correlation_key(&agent(THREAD), namespace, std::slice::from_ref(&token));
            archive.correlations.insert(key.clone(), reference);
            archive.retain_correlation_evidence(
                key,
                vec![ObservationRef {
                    issuer: Issuer::Server("controller".into()),
                    producer_id: "controls".into(),
                    producer_epoch: "one".into(),
                    producer_seq: index,
                }],
            );
        }
    }
    archive
}
fn map_rows(rows: &[Value], archive: &ArchiveContext, gap: bool) -> Vec<MapOutput> {
    let mut state = MapperState::default();
    let mut output = vec![];
    let mut offset = 0;
    for (index, row) in rows.iter().enumerate() {
        let mut raw = serde_json::to_vec(row).unwrap();
        raw.push(b'\n');
        let mut capture = capture(index as u64 + 1, false);
        if gap && row["payload"]["item"]["type"] == "UserMessage" {
            offset += 1;
        }
        capture.source_position.locator = SourceLocator::Jsonl {
            byte_start: offset,
            byte_end: offset + raw.len() as u64,
            record_ordinal: index as u64,
        };
        offset += raw.len() as u64;
        let result = CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &state,
            archive,
        });
        state = result.state.clone();
        output.push(result);
    }
    output
}
#[test]
fn native_input_pairing_requires_complete_coordinates_owner_turn_and_user_content_kind() {
    let rows = records();
    let archive = archive(&agent(THREAD));
    for variation in ["complete", "gap", "turn", "kind"] {
        let mut rows = rows.clone();
        for row in &mut rows {
            let Some(metadata) = row
                .get_mut("payload")
                .and_then(|payload| payload.get_mut("internal_chat_message_metadata_passthrough"))
            else {
                continue;
            };
            if metadata["content_item_kinds"] == serde_json::json!(["user.text"]) {
                if variation == "turn" {
                    metadata["turn_id"] = "unrelated-turn".into();
                }
                if variation == "kind" {
                    metadata["content_item_kinds"] =
                        serde_json::json!(["environments.environment_context"]);
                }
            }
        }
        let outputs = map_rows(&rows, &archive, variation == "gap");
        let claims: Vec<_> = outputs.iter().flat_map(|output| &output.claims).collect();
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(
                    claim.fact,
                    Fact::DeliveryStage {
                        stage: DeliveryStage::Admitted { .. },
                        ..
                    }
                ))
                .count(),
            2
        );
        let paired=claims.iter().filter(|claim|matches!(&claim.fact,Fact::Relation{relation:Relation::Representation{rule,..}}if rule=="codex/record-user-prompt-and-emit-turn-item")).count();
        assert_eq!(
            paired,
            if variation == "complete" { 2 } else { 0 },
            "{variation}: text equality is not a source certificate"
        );
    }
}
#[test]
fn input_correlation_cannot_borrow_a_different_agents_delivery_or_an_absent_dispatch() {
    for context in [
        archive(&agent("different-agent")),
        ArchiveContext::default(),
    ] {
        let outputs = map_rows(&records(), &context, false);
        assert!(
            !outputs
                .iter()
                .flat_map(|output| &output.claims)
                .any(|claim| matches!(
                    claim.fact,
                    Fact::DeliveryStage { .. }
                        | Fact::Relation {
                            relation: Relation::Representation { .. }
                        }
                ))
        );
        assert!(
            outputs
                .iter()
                .flat_map(|output| &output.claims)
                .any(|claim| matches!(
                    claim.fact,
                    Fact::Declare {
                        object: TraceObject::MessageRevision(_)
                    }
                )),
            "unmatched input remains readable as retained vendor content"
        );
    }
}
