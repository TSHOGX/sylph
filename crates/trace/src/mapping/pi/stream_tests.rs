use super::*;
use crate::mapping::{ArchiveContext, MapperState};
use crate::reading::{project, PayloadPreview, PayloadPreviews, ReadingScope};
use crate::reducer::Reducer;
use sylph_protocol::trace::read::{ReadAuthor, ReadAvailability, ReadingContent};
use sylph_protocol::EnvironmentId;

#[derive(Default)]
struct Stream {
    state: MapperState,
    reducer: Reducer,
    payloads: PayloadPreviews,
    outputs: Vec<MapOutput>,
}

impl Stream {
    fn push(&mut self, value: &Value) {
        self.push_source(value, false);
    }

    fn push_source(&mut self, value: &Value, native: bool) {
        let index = self.outputs.len() as u64;
        let issuer = Issuer::Environment(EnvironmentId::parse("env").unwrap());
        let capture = Capture {
            observation_ref: ObservationRef {
                issuer: issuer.clone(),
                producer_id: if native { "pi-native" } else { "pi-rpc" }.into(),
                producer_epoch: "first-process".into(),
                producer_seq: index + 1,
            },
            source_position: SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: if native { "pi-native" } else { "pi-stdout" }.into(),
                },
                generation: 0,
                locator: if native {
                    SourceLocator::Jsonl {
                        byte_start: index * 1024,
                        byte_end: (index + 1) * 1024,
                        record_ordinal: index,
                    }
                } else {
                    SourceLocator::Frame { ordinal: index }
                },
            },
            ingestion_authorization_epoch: IngestionEpoch::new("capture").unwrap(),
            declared_subject_refs: vec![],
            captured_at: TimeEvidence {
                domain: "receipt".into(),
                value: index as i64,
                unit: TimeUnit::Milliseconds,
                precision: None,
                basis: TimeBasis::ServerReceipt,
            },
            source_clock: None,
            transport_kind: if native {
                TransportKind::NativeJsonl
            } else {
                TransportKind::LiveStdout
            },
            vendor_build: Some("0.85.1".into()),
            schema_fingerprint: None,
            payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        let output = PiMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(value).unwrap(),
            state: &self.state,
            archive: &ArchiveContext::default(),
        });
        let evidence = output
            .claims
            .iter()
            .flat_map(|claim| claim.evidence.clone())
            .collect();
        self.reducer.apply(&output.claims, &evidence, 1000).unwrap();
        self.state = serde_json::from_value(serde_json::to_value(&output.state).unwrap()).unwrap();
        for payload in &output.payloads {
            self.payloads.insert(
                payload.reference.clone(),
                PayloadPreview {
                    preview: String::from_utf8(payload.bytes.clone()).unwrap(),
                    preview_limit: 2048,
                    total_bytes: Some(payload.bytes.len() as u64),
                    availability: Some(ReadAvailability::Available),
                    ..PayloadPreview::default()
                }
                .into(),
            );
        }
        self.outputs.push(output);
    }

    fn assistant_rows(&self) -> Vec<Vec<(MessagePartRef, ContentKind, String)>> {
        let projection = project(&self.reducer.resolve(), &self.payloads, 32);
        let scope = ReadingScope {
            agent: self.state.governing_agent.clone().unwrap(),
            selection: None,
        };
        projection.windows[&scope]
            .iter()
            .filter_map(|unit| match &unit.content {
                ReadingContent::Message {
                    author: ReadAuthor::Agent { .. },
                    parts,
                    ..
                } => Some(
                    parts
                        .iter()
                        .map(|part| (part.part_ref.clone(), part.kind, part.preview.clone()))
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    fn assistant_parts(&self) -> Vec<(MessagePartRef, ContentKind, String)> {
        let messages = self.assistant_rows();
        assert_eq!(
            messages.len(),
            1,
            "One assistant message must be readable before message_end"
        );
        messages.into_iter().next().unwrap()
    }
}

fn update(event: Value) -> Value {
    serde_json::json!({"type":"message_update","assistantMessageEvent":event})
}

fn started() -> Stream {
    let mut stream = Stream::default();
    for value in [
        serde_json::json!({"type":"response","command":"get_state","success":true,"data":{"sessionId":"stream"}}),
        serde_json::json!({"type":"agent_start"}),
        serde_json::json!({"type":"message_start","message":{"role":"assistant","content":[]}}),
    ] {
        stream.push(&value);
    }
    stream
}

#[test]
fn recorded_rpc_equal_deltas_are_readable_before_message_end() {
    let frames: Vec<Value> =
        include_str!("../../../../../fixtures/unified-trace/pi/stdout-success.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let mut stream = Stream::default();
    let mut expected = String::new();
    let mut part = None;
    for frame in frames
        .iter()
        .take_while(|frame| frame["type"] != "agent_settled")
    {
        stream.push(frame);
        if frame["assistantMessageEvent"]["type"] == "text_delta" {
            expected.push_str(frame["assistantMessageEvent"]["delta"].as_str().unwrap());
            let parts = stream.assistant_parts();
            assert_eq!(parts.len(), 1);
            assert_eq!(parts[0].2, expected);
            if let Some(part) = &part {
                assert_eq!(&parts[0].0, part);
            }
            part = Some(parts[0].0.clone());
            assert_eq!(
                stream.outputs.last().unwrap().payloads[0].bytes,
                frame["assistantMessageEvent"]["delta"]
                    .as_str()
                    .unwrap()
                    .as_bytes()
            );
        }
        assert!(stream
            .reducer
            .resolve()
            .executions
            .iter()
            .filter(|(reference, _)| matches!(reference, AnyObjectRef::Activation(_)))
            .all(|(_, execution)| execution.outcome.known().is_none()));
    }
    assert_eq!(expected, "SYLPH_PI_AA_OK");
    assert_eq!(stream.assistant_parts()[0].2, expected);
    assert!(stream
        .outputs
        .iter()
        .flat_map(|output| &output.claims)
        .all(|claim| !matches!(claim.fact, Fact::Equivalence { .. })));
    let settled = frames
        .iter()
        .position(|frame| frame["type"] == "agent_settled")
        .unwrap();
    stream.push(&frames[settled]);
    let reduced = stream.reducer.resolve();
    assert_eq!(
        reduced
            .executions
            .keys()
            .filter(|reference| matches!(reference, AnyObjectRef::Activation(_)))
            .count(),
        1
    );
    assert_eq!(
        reduced.executions.values().next().unwrap().outcome.known(),
        Some(&TerminalOutcome::Succeeded)
    );
}

#[test]
fn reasoning_and_text_keep_their_content_indices_through_terminal_replacement() {
    let mut stream = started();
    for event in [
        serde_json::json!({"type":"thinking_start","contentIndex":0}),
        serde_json::json!({"type":"thinking_delta","contentIndex":0,"delta":"Considering"}),
        serde_json::json!({"type":"thinking_end","contentIndex":0,"content":"Considered"}),
        serde_json::json!({"type":"text_start","contentIndex":1}),
        serde_json::json!({"type":"text_delta","contentIndex":1,"delta":"Answer"}),
    ] {
        stream.push(&update(event));
    }
    let before = stream.assistant_parts();
    assert_eq!(
        before
            .iter()
            .map(|part| (part.1, part.2.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (ContentKind::DisclosedReasoning, "Considered"),
            (ContentKind::Text, "Answer")
        ]
    );
    stream.push(&serde_json::json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Considered"},{"type":"text","text":"Rewritten answer"}],"stopReason":"stop"}}));
    let after = stream.assistant_parts();
    assert_eq!(after[0].0, before[0].0);
    assert_eq!(after[1].0, before[1].0);
    assert_eq!(after[1].2, "Rewritten answer");
    assert_ne!(after[0].0, after[1].0);
    assert!(stream
        .reducer
        .resolve()
        .executions
        .iter()
        .filter(|(reference, _)| matches!(reference, AnyObjectRef::Activation(_)))
        .all(|(_, execution)| execution.outcome.known().is_none()));
}

#[test]
fn complete_native_and_rpc_captures_keep_equal_messages_separate_without_a_join() {
    let frames: Vec<Value> =
        include_str!("../../../../../fixtures/unified-trace/pi/stdout-success.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let mut live = Stream::default();
    for frame in &frames {
        live.push(frame);
    }
    let rows = live.assistant_rows();
    assert_eq!(
        rows.len(),
        2,
        "Provisional live content and native admission have no identity certificate"
    );
    assert!(rows.iter().all(|parts| parts[0].2 == "SYLPH_PI_AA_OK"));
    assert_ne!(rows[0][0].0, rows[1][0].0);
    let reduced = live.reducer.resolve();
    let history = &reduced.histories[live.state.governing_agent.as_ref().unwrap()];
    let selected = history.selected_entries.as_ref().unwrap();
    assert!(selected.iter().all(|entry| matches!(entry.address(), ObjectAddress::Native { namespace, .. } if namespace == "pi.entry")));
    assert!(
        matches!(selected.last().unwrap().address(), ObjectAddress::Native { key, .. } if key == &vec![frames.last().unwrap()["data"]["leafId"].as_str().unwrap().to_owned()])
    );

    let mut native = Stream::default();
    for line in
        include_str!("../../../../../fixtures/unified-trace/pi/native-success.jsonl").lines()
    {
        native.push_source(&serde_json::from_str(line).unwrap(), true);
    }
    assert_eq!(native.assistant_parts()[0].2, "SYLPH_PI_AA_OK");
    assert!(native.reducer.resolve().executions.is_empty());
}

#[test]
fn partial_tool_arguments_stay_opaque_until_the_complete_toolcall_arrives() {
    let mut stream = started();
    stream.push(&update(
        serde_json::json!({"type":"toolcall_start","contentIndex":0,"id":"call","toolName":"read"}),
    ));
    stream.push(&update(
        serde_json::json!({"type":"toolcall_delta","contentIndex":0,"delta":"{\"path\":"}),
    ));
    let partial = stream.assistant_parts();
    assert_eq!(partial[0].1, ContentKind::Opaque);
    assert_eq!(partial[0].2, "{\"path\":");
    stream.push(&update(
        serde_json::json!({"type":"toolcall_delta","contentIndex":0,"delta":"\"file\"}"}),
    ));
    assert_eq!(stream.assistant_parts()[0].2, "{\"path\":\"file\"}");
    let tool = serde_json::json!({"type":"toolCall","id":"call","name":"read","arguments":{"path":"file"}});
    stream.push(&update(
        serde_json::json!({"type":"toolcall_end","contentIndex":0,"toolCall":tool}),
    ));
    let complete = stream.assistant_parts();
    assert_eq!(complete[0].0, partial[0].0);
    assert_eq!(complete[0].1, ContentKind::Structured);
    assert_eq!(serde_json::from_str::<Value>(&complete[0].2).unwrap(), tool);
    stream.push(&serde_json::json!({"type":"message_end","message":{"role":"assistant","content":[tool],"stopReason":"toolUse"}}));
    let reduced = stream.reducer.resolve();
    let operations: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|resolved| match resolved.declaration.known() {
            Some(TraceObject::Operation(operation))
                if matches!(operation.category, OperationCategory::Tool { .. }) =>
            {
                Some(operation)
            }
            _ => None,
        })
        .collect();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].argument_state, ArgumentState::Sealed);
    assert!(reduced
        .execution(&AnyObjectRef::Operation(
            operations[0].operation_ref.clone()
        ))
        .is_none_or(|execution| execution.outcome.known().is_none()));
    assert!(reduced
        .executions
        .iter()
        .filter(|(reference, _)| matches!(reference, AnyObjectRef::Activation(_)))
        .all(|(_, execution)| execution.outcome.known().is_none()));
}

#[test]
fn invalid_part_coordinates_and_late_deltas_do_not_replace_a_readable_prefix() {
    let mut stream = started();
    stream.push(&update(
        serde_json::json!({"type":"text_start","contentIndex":0}),
    ));
    stream.push(&update(
        serde_json::json!({"type":"text_delta","contentIndex":0,"delta":"retained"}),
    ));
    stream.push(&update(
        serde_json::json!({"type":"text_end","contentIndex":0,"content":"retained"}),
    ));
    for event in [
        serde_json::json!({"type":"text_start","contentIndex":u64::MAX}),
        serde_json::json!({"type":"text_start","contentIndex":0}),
        serde_json::json!({"type":"text_delta","contentIndex":0,"delta":" late"}),
        serde_json::json!({"type":"thinking_delta","contentIndex":0,"delta":" wrong kind"}),
        serde_json::json!({"type":"text_delta","contentIndex":1,"delta":" missing start"}),
    ] {
        let prior = stream.state.values["pi"]["message"].clone();
        stream.push(&update(event));
        assert_eq!(stream.assistant_parts()[0].2, "retained");
        assert_eq!(stream.state.values["pi"]["message"], prior);
        assert!(stream
            .outputs
            .last()
            .unwrap()
            .claims
            .iter()
            .any(|claim| matches!(claim.fact, Fact::Diagnostic { .. })));
    }
}

#[test]
fn consecutive_messages_reuse_part_indices_without_reusing_part_identity() {
    let mut stream = started();
    stream.push(&update(
        serde_json::json!({"type":"text_start","contentIndex":0}),
    ));
    stream.push(&update(
        serde_json::json!({"type":"text_delta","contentIndex":0,"delta":"first"}),
    ));
    stream.push(&serde_json::json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"first"}],"stopReason":"stop"}}));
    stream.push(
        &serde_json::json!({"type":"message_start","message":{"role":"assistant","content":[]}}),
    );
    stream.push(&update(
        serde_json::json!({"type":"text_start","contentIndex":0}),
    ));
    stream.push(&update(
        serde_json::json!({"type":"text_delta","contentIndex":0,"delta":"second"}),
    ));
    let rows = stream.assistant_rows();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0][0].0, rows[1][0].0);
    assert_eq!(
        rows.iter()
            .map(|parts| parts[0].2.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["first", "second"])
    );
}
