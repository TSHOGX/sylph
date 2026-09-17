//! Independent expectations for the retained s10 app-server frames. Original
//! capture line numbers name observable checkpoints, not transport receipt order.
use super::*;
use crate::trace::reading_store::read_scope;
use sylph_trace::reducer::Resolved;

const PARENT: &str = "01a0462b-b919-7c33-9d9c-f8bec39c3cc8";
const CHILD: &str = "01a0462b-cdb3-75e0-b609-0a218686b187";
const FORK: &str = "01a0462b-f540-7643-ba33-2125d7ad3ec6";
const PARENT_TURN: &str = "01a0462b-b985-7c12-ba38-915f9e0dadf0";
const CHILD_TURN: &str = "01a0462b-cde2-76d1-a20d-3613d4b20a7c";
const SECOND_PARENT_TURN: &str = "01a0462b-f580-70e2-8e73-0f99eedb0f67";
const COMMAND: &str = "exec-d37affc5-516f-4201-96dc-af4b34e8e07b";
const WAIT: &str = "call_XVDJ6uygCi5YWTf8A9KnQg6B";

fn activation(agent: &str, turn: &str) -> ActivationRef {
    ActivationRef::native(owner(agent), "codex.turn", vec![turn.into()]).unwrap()
}

fn own_operation(agent: &str, namespace: &str, id: &str) -> OperationRef {
    OperationRef::native(owner(agent), namespace, vec![id.into()]).unwrap()
}

fn source_position(line: u64) -> SourcePosition {
    SourcePosition {
        source: SourceRef {
            issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
            instance: "retained-delegation-live".into(),
        },
        generation: 0,
        locator: SourceLocator::Frame { ordinal: line },
    }
}

fn assert_execution(reduced: &Reduction, execution: &AnyObjectRef, ended: bool) {
    let state = reduced
        .execution(execution)
        .expect("the observed boundary declares this execution");
    assert!(
        !state.started.is_empty(),
        "{execution:?} needs its own start evidence"
    );
    if ended {
        assert!(
            !state.terminated.is_empty(),
            "{execution:?} needs its own termination evidence"
        );
        assert_eq!(state.outcome.known(), Some(&TerminalOutcome::Succeeded));
    } else {
        assert!(
            state.terminated.is_empty(),
            "another execution cannot close {execution:?}"
        );
        assert_eq!(state.outcome, Resolved::Unknown);
    }
}

fn assert_activation(reduced: &Reduction, agent: &str, turn: &str, ended: bool) {
    let reference = activation(agent, turn);
    let object = AnyObjectRef::Activation(reference.clone());
    let Some(TraceObject::Activation(value)) = reduced
        .object(&object)
        .and_then(|value| value.declaration.known())
    else {
        panic!("the vendor's native turn ID must identify its own activation");
    };
    assert_eq!(value.activation_ref, reference);
    assert_eq!(
        value.owner,
        OwnerRef::Agent {
            agent: owner(agent)
        }
    );
    assert_execution(reduced, &object, ended);
}

fn assert_clock(reduced: &Reduction, agent: &str, turn: &str, started: i64, completed: i64) {
    let state = reduced
        .execution(&AnyObjectRef::Activation(activation(agent, turn)))
        .unwrap();
    for (is_start, seconds) in [(true, started), (false, completed)] {
        assert!(
            state.boundaries.iter().any(|(boundary, _)| {
                let time = match boundary {
                    ExecutionBoundary::Started { time } if is_start => time,
                    ExecutionBoundary::Terminated { time, .. } if !is_start => time,
                    _ => return false,
                };
                time.as_ref().is_some_and(|time| {
                    time.value == seconds
                        && time.unit == TimeUnit::Seconds
                        && time.basis == TimeBasis::SourceWall
                })
            }),
            "activation clocks come from Unix seconds, not frame receipt milliseconds"
        );
    }
}

async fn header(store: &GenerationStore, agent: &AgentRef) -> AgentHeader {
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    serde_json::from_value(
        store
            .read_row(
                &snapshot,
                &read_scope(agent, None, "header"),
                &agent.encode(),
            )
            .await
            .unwrap()
            .body,
    )
    .unwrap()
}

async fn assert_answer(
    fixture: &Fixture,
    store: &GenerationStore,
    reduced: &Reduction,
    agent: &str,
    id: &str,
    expected: &[u8],
) {
    let message = MessageRef::native(owner(agent), "codex.message", vec![id.into()]).unwrap();
    let Some(TraceObject::Message(declaration)) = reduced
        .object(&AnyObjectRef::Message(message.clone()))
        .and_then(|value| value.declaration.known())
    else {
        panic!("the observed final message must retain its native item identity");
    };
    assert_eq!(
        declaration.author,
        Author::Agent {
            agent: OwnerRef::Agent {
                agent: owner(agent)
            }
        }
    );
    assert_eq!(declaration.purpose, MessagePurpose::Answer);
    assert_eq!(
        declaration.production.as_ref().unwrap().owner,
        OwnerRef::Agent {
            agent: owner(agent)
        }
    );
    let selected = reduced.messages[&message].selected.known().unwrap();
    let Some(TraceObject::MessageRevision(revision)) = reduced
        .object(&AnyObjectRef::MessageRevision(selected.clone()))
        .and_then(|value| value.declaration.known())
    else {
        panic!("final content is an immutable revision");
    };
    assert!(matches!(revision.form, RevisionForm::Final { .. }));
    let mut bytes = Vec::new();
    for part in &revision.parts {
        bytes.extend(
            fixture
                .evidence
                .payload_bytes(&part.payload, 1024)
                .await
                .unwrap(),
        );
    }
    assert_eq!(bytes, expected);
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let page = store
        .read_page(
            &snapshot,
            &read_scope(&owner(agent), None, "history"),
            None,
            None,
            None,
            store.limits().max_items,
            false,
        )
        .await
        .unwrap();
    assert!(!page.has_after && !page.work_exhausted);
    let rows: Vec<ReadingUnit> = page
        .rows
        .into_iter()
        .map(|row| serde_json::from_value(row.body).unwrap())
        .collect();
    assert_eq!(rows.iter().filter(|row| matches!(&row.content, ReadingContent::Message { message_ref, .. } if message_ref == &message)).count(), 1,
        "started, delta and completed frames render one message at its stable location");
}

pub(super) async fn assert_expected(
    fixture: &Fixture,
    store: &GenerationStore,
    through_line: usize,
) {
    let claims = claims(store).await;
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.iter().cloned())
        .collect();
    let mut reducer = Reducer::default();
    reducer.apply(&claims, &evidence, claims.len()).unwrap();
    let reduced = reducer.resolve();

    let mut expected_agents = BTreeSet::from([owner(PARENT)]);
    if through_line >= 19 {
        expected_agents.insert(owner(CHILD));
    }
    if through_line >= 83 {
        expected_agents.insert(owner(FORK));
    }
    let actual_agents: BTreeSet<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Agent(agent)) if agent.agent_ref != owner("response-user") => {
                Some(agent.agent_ref.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        actual_agents, expected_agents,
        "activity IDs, command IDs and completion notices never become phantom agents"
    );
    assert!(
        !reduced
            .relations
            .iter()
            .any(|relation| matches!(relation.relation, Relation::Spawn { .. })),
        "activity alone is not spawn proof, and forkedFromId is not parentThreadId"
    );
    assert!(
        reduced.delivery_stages.is_empty(),
        "these frames carry no proved task admission or inference inclusion"
    );
    assert!(
        reduced
            .objects
            .values()
            .all(|object| !matches!(object.declaration.known(), Some(TraceObject::Delivery(_)))),
        "an empty wait target list or a generic activity is not delivery evidence"
    );

    let mut expected_activations = BTreeSet::from([activation(PARENT, PARENT_TURN)]);
    assert_activation(&reduced, PARENT, PARENT_TURN, through_line >= 72);
    if through_line >= 22 {
        expected_activations.insert(activation(CHILD, CHILD_TURN));
        assert_activation(&reduced, CHILD, CHILD_TURN, through_line >= 95);
    }
    if through_line >= 87 {
        expected_activations.insert(activation(PARENT, SECOND_PARENT_TURN));
        assert_activation(&reduced, PARENT, SECOND_PARENT_TURN, false);
    }
    let actual_activations: BTreeSet<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Activation(activation))
                if activation.activation_ref.scope() != &owner("response-user") =>
            {
                Some(activation.activation_ref.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(actual_activations, expected_activations);
    if through_line >= 72 {
        assert_clock(&reduced, PARENT, PARENT_TURN, 1_787_883_665, 1_787_883_680);
    }
    if through_line >= 95 {
        assert_clock(&reduced, CHILD, CHILD_TURN, 1_787_883_671, 1_787_883_681);
    }

    let wait = own_operation(PARENT, "codex.collaboration-item", WAIT);
    let command = own_operation(CHILD, "codex.runtime-item", COMMAND);
    let mut expected_operations = BTreeSet::new();
    if through_line >= 29 {
        expected_operations.insert(wait.clone());
        let Some(TraceObject::Operation(operation)) = reduced
            .object(&AnyObjectRef::Operation(wait.clone()))
            .and_then(|value| value.declaration.known())
        else {
            panic!("observed wait operation");
        };
        assert_eq!(operation.category, OperationCategory::Wait);
        assert_eq!(
            operation.owner,
            OwnerRef::Agent {
                agent: owner(PARENT)
            }
        );
        assert_eq!(operation.activation, Some(activation(PARENT, PARENT_TURN)));
        assert_execution(
            &reduced,
            &AnyObjectRef::Operation(wait.clone()),
            through_line >= 38,
        );
    }
    if through_line >= 30 {
        expected_operations.insert(command.clone());
        let Some(TraceObject::Operation(operation)) = reduced
            .object(&AnyObjectRef::Operation(command.clone()))
            .and_then(|value| value.declaration.known())
        else {
            panic!("observed child command operation");
        };
        assert_eq!(
            operation.owner,
            OwnerRef::Agent {
                agent: owner(CHILD)
            }
        );
        assert_eq!(operation.activation, Some(activation(CHILD, CHILD_TURN)));
        assert_eq!(
            operation.category,
            OperationCategory::TerminalIo {
                action: TerminalAction::Launch
            }
        );
        assert_eq!(operation.argument_state, ArgumentState::Sealed);
        assert!(
            operation.requester.is_none(),
            "the frames do not identify an enclosing code cell"
        );
        assert_eq!(
            fixture
                .evidence
                .payload_bytes(operation.input_revision.as_ref().unwrap(), 1024)
                .await
                .unwrap(),
            b"/bin/zsh -lc 'echo from-subagent'"
        );
        assert_execution(
            &reduced,
            &AnyObjectRef::Operation(command.clone()),
            through_line >= 31,
        );
        let resource = ResourceRef::native(
            owner(CHILD),
            "codex.process-creation",
            vec![COMMAND.into(), "97046".into()],
        )
        .unwrap();
        let Some(TraceObject::Resource(value)) = reduced
            .object(&AnyObjectRef::Resource(resource.clone()))
            .and_then(|value| value.declaration.known())
        else {
            panic!("process handle has an independent resource");
        };
        assert_eq!(value.creator, Some(command.clone()));
        assert_eq!(
            value.host,
            Some(OwnerRef::Agent {
                agent: owner(CHILD)
            })
        );
        assert!(
            matches!(&value.kind, ResourceKind::Terminal { native_handle, .. } if native_handle == "97046")
        );
        let states = &reduced.resource_states[&resource];
        assert!(states
            .iter()
            .any(|(state, _)| *state == ResourceState::Running));
        assert_eq!(
            states
                .iter()
                .filter(|(state, _)| matches!(state, ResourceState::Closed { .. }))
                .count(),
            usize::from(through_line >= 31)
        );
        if through_line >= 31 {
            assert!(states.iter().any(|(state, _)| matches!(
                state,
                ResourceState::Closed {
                    outcome: TerminalOutcome::Succeeded,
                    reason: Some(ExecutionCloseReason::VendorStop)
                }
            )));
            let reference = ResultRef::occurrence(
                owner(CHILD),
                source_position(31),
                vec!["runtime-result".into()],
            )
            .unwrap();
            let Some(TraceObject::Result(result)) = reduced
                .object(&AnyObjectRef::Result(reference))
                .and_then(|value| value.declaration.known())
            else {
                panic!("runtime result identity preserves the original captured frame");
            };
            assert_eq!(result.operation, command.clone());
            assert_eq!(result.representation, Representation::Runtime);
            assert_eq!(result.disposition, ResultDisposition::FinalReturn);
            assert!(result.recipient.is_none() && result.delivery.is_none());
            assert_eq!(
                fixture
                    .evidence
                    .payload_bytes(result.payload.as_ref().unwrap(), 1024)
                    .await
                    .unwrap(),
                b"from-subagent\n"
            );
        }
        assert_eq!(reduced.resource_states.len(), 1);
    } else {
        assert!(reduced.resource_states.is_empty());
    }
    let actual_operations: BTreeSet<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Operation(operation)) => Some(operation.operation_ref.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(actual_operations, expected_operations);

    if through_line >= 61 {
        assert_answer(
            fixture,
            store,
            &reduced,
            PARENT,
            "msg_0408278a4c00528a016a90f0a058fc87d1bf3cf5764edd10e9",
            b"The subagent ran the command and reported:\n\n`from-subagent`",
        )
        .await;
    }
    if through_line >= 91 {
        assert_answer(
            fixture,
            store,
            &reduced,
            CHILD,
            "msg_01d7f6b4d9bc127d016a90f0a0bd9087d1ba3cd8c795745ffd",
            b"The command output was exactly:\n\n`from-subagent`",
        )
        .await;
    }
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    for agent in expected_agents {
        let value = header(store, &agent).await;
        assert_eq!(value.agent.address.agent(), agent);
        assert!(value.parent.is_none());
        assert_eq!(value.descendant_count.value, Some(0));
        assert!(value.descendant_count.complete);
        assert!(
            !value.history.selection_known,
            "the capture has no selected-head observation"
        );
        let relations = store
            .read_page(
                &snapshot,
                &read_scope(&agent, None, "relations"),
                None,
                None,
                None,
                16,
                false,
            )
            .await
            .unwrap();
        assert!(relations.rows.is_empty() && !relations.has_after && !relations.work_exhausted);
        assert_eq!(relations.total, Some(0));
        if agent == owner(PARENT) && through_line >= 87 {
            assert_eq!(
                value.own_execution.reference,
                Some(TraceAddress::of(&AnyObjectRef::Activation(activation(
                    PARENT,
                    SECOND_PARENT_TURN
                ))))
            );
            assert!(!matches!(
                value.own_execution.state,
                ReadExecutionState::Terminated { .. }
            ));
        } else if (agent == owner(PARENT) && through_line >= 72)
            || (agent == owner(CHILD) && through_line >= 95)
        {
            assert!(matches!(
                value.own_execution.state,
                ReadExecutionState::Terminated {
                    outcome: TerminalOutcome::Succeeded,
                    ..
                }
            ));
        } else if agent == owner(FORK) || (agent == owner(CHILD) && through_line < 22) {
            assert_eq!(value.own_execution.state, ReadExecutionState::Unavailable);
        } else {
            assert!(!matches!(
                value.own_execution.state,
                ReadExecutionState::Terminated { .. }
            ));
        }
    }
}
