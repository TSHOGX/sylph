use super::*;

const NATIVE: &str =
    include_str!("../../../../../../fixtures/unified-trace/codex/goal-context/native.jsonl");
const MANIFEST: &str =
    include_str!("../../../../../../fixtures/unified-trace/codex/goal-context/capture.json");
fn manifest() -> Value {
    serde_json::from_str(MANIFEST).unwrap()
}
fn rows() -> Vec<Value> {
    NATIVE
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn message<'a>(reduced: &'a crate::reducer::Reduction, id: &str) -> &'a Message {
    let owner = agent(manifest()["session_id"].as_str().unwrap());
    let reference = MessageRef::native(owner, "codex.message", vec![id.into()]).unwrap();
    let Some(TraceObject::Message(message)) = reduced.objects[&AnyObjectRef::Message(reference)]
        .declaration
        .known()
    else {
        panic!("recorded message must remain known")
    };
    message
}
#[test]
fn recorded_goal_context_and_identical_ordinary_input_keep_different_purposes_and_authors() {
    let records = rows();
    let output = run(&records, false);
    let reduced = reduce(&output);
    let expected = manifest();
    let goal_ids = expected["observed"]["goal_context_message_ids"]
        .as_array()
        .unwrap();
    for id in goal_ids {
        let item = message(&reduced, id.as_str().unwrap());
        assert_eq!(item.protocol_role, Some(ProtocolRole::User));
        assert_eq!(item.purpose, MessagePurpose::Context);
        assert_eq!(
            item.author,
            Author::Harness {
                identity: Some("codex".into())
            }
        );
    }
    let notice = message(&reduced, "msg_01a0cbf1-b562-7321-b225-d745fec934ff");
    assert_eq!(notice.purpose, MessagePurpose::Notice);
    assert_eq!(
        notice.author,
        Author::Harness {
            identity: Some("codex".into())
        }
    );
    let typed_input = message(&reduced, "01a0cbf1-b578-74d0-b3f6-54757f23138a");
    assert_eq!(typed_input.purpose, MessagePurpose::TaskInput);
    assert_eq!(typed_input.author, Author::Unknown);
    let literal_id = expected["observed"]["literal_message_id"].as_str().unwrap();
    let literal = message(&reduced, literal_id);
    assert_eq!(literal.protocol_role, Some(ProtocolRole::User));
    assert_eq!(literal.purpose, MessagePurpose::TaskInput);
    assert_eq!(
        literal.author,
        Author::Unknown,
        "a client input annotation does not identify a human Subject"
    );
    let body = |id: &str| {
        records
            .iter()
            .find(|record| record["payload"]["id"].as_str() == Some(id))
            .unwrap()["payload"]["content"][0]["text"]
            .as_str()
            .unwrap()
    };
    let same = expected["observed"]["equal_goal_text_message_ids"][0]
        .as_str()
        .unwrap();
    assert_eq!(body(same), body(literal_id));
    assert_ne!(message(&reduced, same).message_ref, literal.message_ref);
    assert!(output
        .iter()
        .flat_map(|output| &output.claims)
        .all(|claim| !matches!(
            claim.fact,
            Fact::Relation {
                relation: Relation::Trigger { .. }
            }
        )));
}

fn altered(annotation: Value, content: Value, phase: Option<&str>) -> Message {
    let mut value = serde_json::json!({"type":"response_item","ordinal":1,"payload":{"type":"message","id":"annotation","role":"user","content":content,"internal_chat_message_metadata_passthrough":{"content_item_kinds":annotation}}});
    if let Some(phase) = phase {
        value["payload"]["phase"] = phase.into();
    }
    let output = run(&[header(), value], false);
    output
        .into_iter()
        .flat_map(|output| output.claims)
        .find_map(|claim| match claim.fact {
            Fact::Declare {
                object: TraceObject::Message(message),
            } => Some(message),
            _ => None,
        })
        .unwrap()
}
#[test]
fn only_complete_aligned_text_annotations_establish_harness_context() {
    let text =
        serde_json::json!([{"type":"input_text","text":"<goal_context>same text</goal_context>"}]);
    for invalid in [
        Value::Null,
        serde_json::json!([]),
        serde_json::json!(["goal.internal_context", "goal.internal_context"]),
        serde_json::json!([null]),
        serde_json::json!(["future.unknown"]),
        serde_json::json!([".internal_context"]),
        serde_json::json!(["1goal.internal_context"]),
    ] {
        let message = altered(invalid, text.clone(), None);
        assert_eq!(message.author, Author::Unknown);
        assert_eq!(message.purpose, MessagePurpose::Unclassified);
    }
    let media = altered(
        serde_json::json!(["goal.internal_context"]),
        serde_json::json!([{"type":"input_image","image_url":"data:image/png;base64,AA=="}]),
        None,
    );
    assert_eq!(media.author, Author::Unknown);
    assert_eq!(media.purpose, MessagePurpose::Unclassified);
    let valid = altered(
        serde_json::json!(["goal.internal_context"]),
        serde_json::json!([{ "type":"input_text", "text":"annotation without a text wrapper" }]),
        None,
    );
    assert_eq!(
        valid.author,
        Author::Harness {
            identity: Some("codex".into())
        }
    );
    assert_eq!(valid.purpose, MessagePurpose::Context);
}
#[test]
fn a_reported_output_phase_cannot_turn_user_input_into_an_answer() {
    let text = serde_json::json!([{"type":"input_text","text":"ordinary input"}]);
    let unclassified = altered(Value::Null, text.clone(), Some("final_answer"));
    assert_eq!(unclassified.purpose, MessagePurpose::Unclassified);
    let input = altered(serde_json::json!(["user.text"]), text, Some("final_answer"));
    assert_eq!(input.purpose, MessagePurpose::TaskInput);
    assert_eq!(input.author, Author::Unknown);
}
#[test]
fn mixed_harness_purposes_keep_authorship_without_inventing_one_purpose() {
    let text = serde_json::json!([{"type":"input_text","text":"context"},{"type":"input_text","text":"notice"}]);
    let mixed = altered(
        serde_json::json!(["goal.internal_context", "generic.turn_aborted"]),
        text.clone(),
        None,
    );
    assert_eq!(
        mixed.author,
        Author::Harness {
            identity: Some("codex".into())
        }
    );
    assert_eq!(mixed.purpose, MessagePurpose::Unclassified);
    let partly_user = altered(
        serde_json::json!(["goal.internal_context", "user.text"]),
        text,
        None,
    );
    assert_eq!(partly_user.author, Author::Unknown);
    assert_eq!(partly_user.purpose, MessagePurpose::Unclassified);
}
