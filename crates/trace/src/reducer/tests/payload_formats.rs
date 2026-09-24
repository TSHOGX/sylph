use super::*;

fn declarations(media: Option<&str>, encoding: Option<PayloadEncoding>) -> Vec<TraceObject> {
    let payload = PayloadUse {
        reference: PayloadRef::bytes("a".repeat(64)).unwrap(),
        format: PayloadFormat {
            media_type: media.map(str::to_owned),
            encoding,
        },
    };
    let operation =
        OperationRef::native(agent("root"), "operation", vec!["format".into()]).unwrap();
    vec![
        TraceObject::Operation(Operation {
            operation_ref: operation.clone(),
            owner: OwnerRef::Agent {
                agent: agent("root"),
            },
            activation: None,
            requester: None,
            category: OperationCategory::Wait,
            input_revision: Some(payload.clone()),
            argument_state: ArgumentState::Sealed,
        }),
        TraceObject::Result(ResultOccurrence {
            result_ref: ResultRef::native(agent("root"), "result", vec!["format".into()]).unwrap(),
            operation,
            recipient: None,
            disposition: ResultDisposition::FinalReturn,
            representation: ResultRepresentation::CallerResult,
            payload: Some(payload),
            delivery: None,
        }),
    ]
}

#[test]
fn payload_use_format_refines_absence_but_never_overwrites_disagreeing_known_fields() {
    for reverse in [false, true] {
        for (unknown, known) in declarations(None, None).into_iter().zip(declarations(
            Some("text/plain"),
            Some(PayloadEncoding::Utf8),
        )) {
            let mut pair = [unknown, known.clone()];
            if reverse {
                pair.reverse();
            }
            let mut claims = vec![declared_agent(0, "root")];
            claims.extend(
                pair.into_iter()
                    .enumerate()
                    .map(|(i, object)| claim(i as u64 + 1, Fact::Declare { object })),
            );
            assert_eq!(
                reduce(&claims).objects[&known.object_ref()]
                    .declaration
                    .known(),
                Some(&known)
            );
            for conflict in [
                declarations(Some("application/json"), Some(PayloadEncoding::Utf8)),
                declarations(Some("text/plain"), Some(PayloadEncoding::Binary)),
            ] {
                let object = conflict
                    .into_iter()
                    .find(|object| object.object_ref() == known.object_ref())
                    .unwrap();
                let mut disputed = claims.clone();
                disputed.push(claim(9, Fact::Declare { object }));
                assert!(matches!(
                    reduce(&disputed).objects[&known.object_ref()].declaration,
                    Resolved::Conflicted { .. }
                ));
            }
        }
    }
}
