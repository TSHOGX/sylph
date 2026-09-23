use super::*;

fn declarations(owner: OwnerRef) -> Vec<TraceObject> {
    vec![
        TraceObject::Activation(Activation {
            activation_ref: activation("root", "owned"),
            owner: owner.clone(),
        }),
        TraceObject::Message(Message {
            message_ref: MessageRef::native(agent("root"), "message", vec!["owned".into()])
                .unwrap(),
            author: Author::Agent {
                agent: owner.clone(),
            },
            protocol_role: Some(ProtocolRole::Assistant),
            purpose: MessagePurpose::Unclassified,
            production: Some(ProductionScope {
                owner: owner.clone(),
                activation: None,
            }),
        }),
        TraceObject::Operation(Operation {
            operation_ref: OperationRef::native(agent("root"), "operation", vec!["owned".into()])
                .unwrap(),
            owner: owner.clone(),
            activation: None,
            requester: None,
            category: OperationCategory::Tool {
                namespace: None,
                name: "read".into(),
            },
            input_revision: None,
            argument_state: ArgumentState::Sealed,
        }),
        TraceObject::ContextCheckpoint(ContextCheckpoint {
            checkpoint_ref: CheckpointRef::native(
                agent("root"),
                "checkpoint",
                vec!["owned".into()],
            )
            .unwrap(),
            owner: owner.clone(),
            basis: ContextBasis::Summary,
            content: ContextContent::Unknown {
                reason: "Summary unavailable".into(),
            },
            coverage: InclusionCoverage::Unknown {
                reason: "Coverage unavailable".into(),
            },
        }),
        TraceObject::Resource(Resource {
            resource_ref: ResourceRef::native(agent("root"), "resource", vec!["owned".into()])
                .unwrap(),
            creator: None,
            host: Some(owner.clone()),
            kind: ResourceKind::Artifact { media_type: None },
        }),
        TraceObject::HistoryEntry(HistoryEntry {
            entry_ref: entry("owned"),
            containing_agent: agent("root"),
            predecessor: None,
            original_owner: Some(owner),
        }),
    ]
}

fn unresolved(key: &str) -> OwnerRef {
    OwnerRef::Unresolved {
        scope: agent("root"),
        namespace: "native-owner".into(),
        native_key: vec![key.into()],
        reason: "Binding unavailable".into(),
    }
}

#[test]
fn absent_ownership_refines_without_inventing_an_actor_dependency() {
    for observed in [
        OwnerRef::Agent {
            agent: agent("root"),
        },
        unresolved("parent"),
    ] {
        for reverse in [false, true] {
            let unknown = declarations(OwnerRef::Unknown);
            let known = declarations(observed.clone());
            for (mut a, mut b) in unknown.into_iter().zip(known) {
                let expected = b.clone();
                if reverse {
                    std::mem::swap(&mut a, &mut b);
                }
                let mut claims = vec![
                    declared_agent(0, "root"),
                    claim(1, Fact::Declare { object: a.clone() }),
                ];
                assert_eq!(
                    reduce(&claims).objects[&a.object_ref()].declaration.known(),
                    Some(&a)
                );
                claims.push(claim(2, Fact::Declare { object: b.clone() }));
                let reduced = reduce(&claims);
                assert_eq!(
                    reduced.objects[&a.object_ref()].declaration.known(),
                    Some(&expected)
                );
            }
        }
    }
}

#[test]
fn absent_ownership_never_bridges_incompatible_actors_or_native_handles() {
    for (a, b) in [
        (
            OwnerRef::Agent {
                agent: agent("root"),
            },
            OwnerRef::Agent {
                agent: agent("child"),
            },
        ),
        (unresolved("parent"), unresolved("other-parent")),
        (
            unresolved("parent"),
            OwnerRef::Agent {
                agent: agent("root"),
            },
        ),
    ] {
        for ((first, second), unknown) in declarations(a)
            .into_iter()
            .zip(declarations(b))
            .zip(declarations(OwnerRef::Unknown))
        {
            let reference = first.object_ref();
            let declarations = [first, second, unknown];
            // Move the absence claim through every position. It cannot erase a
            // conflict regardless of declaration ordering or incremental arrival.
            for order in [[0, 1, 2], [2, 0, 1], [1, 2, 0]] {
                let mut claims = vec![declared_agent(0, "root"), declared_agent(1, "child")];
                claims.extend(order.into_iter().enumerate().map(|(i, index)| {
                    claim(
                        i as u64 + 2,
                        Fact::Declare {
                            object: declarations[index].clone(),
                        },
                    )
                }));
                assert!(matches!(
                    reduce(&claims).objects[&reference].declaration,
                    Resolved::Conflicted { .. }
                ));
            }
        }
    }
}
