use super::*;
use crate::mapping::{ArchiveContext, MapInput, MapperState, SemanticMapper, codex::CodexMapper};
use serde_json::{Value, json};
use sylph_protocol::{EnvironmentId, SessionId};

#[derive(Clone)]
struct Live {
    reducer: Reducer,
    state: MapperState,
    ordinal: u64,
}

fn agent() -> AgentRef {
    AgentRef::Session(SessionId::decode("env:codex:root").unwrap())
}
fn message() -> MessageRef {
    MessageRef::native(agent(), "codex.message", vec!["stream".into()]).unwrap()
}
fn delta(text: &str) -> Value {
    json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"turn","itemId":"stream","delta":text}})
}
fn item(method: &str, id: &str, text: &str, final_answer: bool) -> Value {
    let mut item = json!({"id":id,"type":"agentMessage","text":text});
    if final_answer {
        item["phase"] = json!("final_answer");
    }
    json!({"method":method,"params":{"threadId":"root","turnId":"turn","item":item}})
}
fn apply(reducer: &mut Reducer, claims: &[Claim]) {
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.iter().cloned())
        .collect();
    reducer.apply(claims, &evidence, claims.len()).unwrap();
}
impl Live {
    fn new(archive_len: usize) -> Self {
        let mut live = Self {
            reducer: Reducer::default(),
            state: MapperState::default(),
            ordinal: 0,
        };
        for frame in [
            json!({"method":"thread/started","params":{"thread":{"id":"root"}}}),
            json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"turn"}}}),
            json!({"method":"unmapped/retained","params":{"threadId":"root"}}),
        ] {
            let claims = live.map(frame);
            apply(&mut live.reducer, &claims);
        }
        for index in 0..archive_len {
            let claims = live.map(item(
                "item/completed",
                &format!("other-{index}"),
                "unrelated",
                true,
            ));
            apply(&mut live.reducer, &claims);
        }
        let claims = live.map(delta("A中"));
        apply(&mut live.reducer, &claims);
        live.reducer.resolve_incremental(100_000);
        live
    }
    fn map(&mut self, frame: Value) -> Vec<Claim> {
        self.ordinal += 1;
        let issuer = Issuer::Environment(EnvironmentId::parse("env").unwrap());
        let raw = serde_json::to_vec(&frame).unwrap();
        let capture = Capture {
            observation_ref: ObservationRef {
                issuer: issuer.clone(),
                producer_id: "live".into(),
                producer_epoch: "epoch".into(),
                producer_seq: self.ordinal,
            },
            source_position: SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: "live".into(),
                },
                generation: 0,
                locator: SourceLocator::Frame {
                    ordinal: self.ordinal,
                },
            },
            ingestion_authorization_epoch: IngestionEpoch::new("admission").unwrap(),
            declared_subject_refs: vec![agent()],
            captured_at: TimeEvidence {
                domain: "receipt".into(),
                value: self.ordinal as i64,
                unit: TimeUnit::Milliseconds,
                precision: None,
                basis: TimeBasis::ServerReceipt,
            },
            source_clock: None,
            transport_kind: TransportKind::LiveStdout,
            vendor_build: None,
            schema_fingerprint: None,
            payload_ref: PayloadRef::bytes(blake3::hash(&raw).to_hex()).unwrap(),
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        let output = CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &self.state,
            archive: &ArchiveContext::default(),
        });
        self.state = output.state;
        assert!(
            !output.claims.is_empty(),
            "the production mapper must interpret the fixture"
        );
        output.claims
    }
    fn next(&mut self, frame: Value) -> Resolution {
        let claims = self.map(frame);
        apply(&mut self.reducer, &claims);
        self.reducer.resolve_incremental(100_000)
    }
}

#[test]
fn mapped_stream_revisions_keep_activation_provenance_final_purpose_and_late_segments() {
    let mut all_work = Vec::new();
    for archive_len in [20, 200] {
        let mut live = Live::new(archive_len);
        let initial = live.reducer.resolve_incremental(100_000).reduction;
        let unrelated = initial.diagnostics.clone();
        assert_eq!(unrelated.len(), 1);
        let old_claims = match &initial.objects[&AnyObjectRef::Message(message())].declaration {
            Resolved::Known { claims, .. } => claims.len(),
            _ => panic!("message declaration known"),
        };
        let mut previous = initial;
        let mut work = Vec::new();
        let mut sealed = None;
        for (index, frame) in [
            delta("A中"),
            item("item/started", "stream", "B", false),
            delta("C"),
            item("item/completed", "stream", "Final differs", true),
            delta("late"),
        ]
        .into_iter()
        .enumerate()
        {
            let resolution = live.next(frame);
            let proof = resolution
                .message_update
                .as_ref()
                .expect("mapped existing-message update is certified");
            assert!(proof.matches(&previous, &resolution.reduction));
            assert!(
                resolution.append.is_none(),
                "existing messages never borrow the independent-tail proof"
            );
            assert_eq!(resolution.work.full_resolutions, 0);
            assert_eq!(resolution.work.history_entries_updated, 1);
            assert_eq!(*resolution.reduction, live.reducer.resolve());
            assert_eq!(
                resolution.changes,
                ReductionChanges::between(&previous, &resolution.reduction)
            );
            assert_eq!(proof.message(), &message());
            assert!(proof.activation().is_some());
            assert!(
                proof
                    .updated_objects()
                    .contains(&AnyObjectRef::Message(message()))
            );
            let after = &resolution.reduction;
            let Resolved::Known {
                value: TraceObject::Message(declaration),
                claims,
            } = &after.objects[&AnyObjectRef::Message(message())].declaration
            else {
                panic!("message remains known");
            };
            assert_eq!(
                claims.len(),
                old_claims + index + 1,
                "each observation preserves its Message provenance"
            );
            let history = &after.messages[&message()];
            assert_eq!(history.revisions.len(), index + 2);
            assert_eq!(history.sealed, index >= 3);
            assert_eq!(history.late_segments.len(), usize::from(index == 4));
            assert_eq!(
                declaration.purpose,
                if index >= 3 {
                    MessagePurpose::Answer
                } else {
                    MessagePurpose::Unclassified
                }
            );
            if index == 3 {
                sealed = history.selected.known().cloned();
            }
            if index == 4 {
                assert_eq!(history.selected.known(), sealed.as_ref());
                assert_eq!(after.diagnostics.len(), unrelated.len() + 1);
                assert!(
                    after
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic == &unrelated[0])
                );
                assert_eq!(
                    after
                        .diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.code == DiagnosticCode::LateSegment)
                        .count(),
                    1
                );
            }
            work.push(resolution.work);
            previous = resolution.reduction;
        }
        all_work.push(work);
    }
    assert_eq!(
        all_work[0], all_work[1],
        "unrelated archive growth must not increase message-local resolution work"
    );
}

#[test]
fn message_update_proofs_compose_only_exact_message_endpoints() {
    let mut live = Live::new(3);
    let before = live.reducer.resolve_incremental(100_000).reduction;
    let first = live.next(delta("B"));
    let second = live.next(delta("C"));
    let a = first.message_update.unwrap();
    let b = second.message_update.unwrap();
    let equal_copy = before.as_ref().clone();
    assert!(!a.matches(&equal_copy, &first.reduction));
    assert!(b.clone().compose(a.clone()).is_none());
    let combined = a.compose(b).unwrap();
    assert!(combined.matches(&before, &second.reduction));
    assert_eq!(combined.agents()[&agent()].added_entries().len(), 2);
    let mut restored: Reducer =
        serde_json::from_slice(&serde_json::to_vec(&live.reducer).unwrap()).unwrap();
    assert!(
        restored
            .resolve_incremental(100_000)
            .message_update
            .is_none()
    );
}

#[test]
fn message_updates_reconcile_prior_references_selections_and_changed_associations() {
    for guard in [
        "future-object",
        "future-claim",
        "selection",
        "activation",
        "correction",
        "sibling",
        "multiple",
    ] {
        let mut live = Live::new(2);
        let mut pending = live.map(delta("B"));
        let next_revision = pending
            .iter()
            .find_map(|claim| match &claim.fact {
                Fact::Declare {
                    object: TraceObject::MessageRevision(revision),
                } => Some((revision.revision_ref.clone(), claim.claim_ref.clone())),
                _ => None,
            })
            .unwrap();
        let mut barrier = pending[0].clone();
        barrier.claim_ref = ClaimRef::native(agent(), "guard", vec![guard.into()]).unwrap();
        barrier.supersedes.clear();
        match guard {
            "future-object" => {
                barrier.fact = Fact::Coverage {
                    coverage: Coverage {
                        subject: AnyObjectRef::MessageRevision(next_revision.0.clone()),
                        source: claim_position(&pending[0]).unwrap().source.clone(),
                        dimension: CoverageDimension::Messages,
                        interval: CoverageInterval::Object {
                            object: AnyObjectRef::MessageRevision(next_revision.0),
                        },
                        state: CoverageState::Following,
                    },
                }
            }
            "future-claim" => {
                barrier.fact = Fact::Diagnostic {
                    diagnostic: TraceDiagnostic {
                        subject: None,
                        code: DiagnosticCode::MissingSource,
                        message: "Future claim is referenced".into(),
                        related_claims: vec![next_revision.1],
                    },
                }
            }
            "selection" => {
                barrier.fact = Fact::Declare {
                    object: TraceObject::HistorySelection(HistorySelection {
                        selection_ref: SelectionRef::native(
                            agent(),
                            "selected",
                            vec!["head".into()],
                        )
                        .unwrap(),
                        agent: agent(),
                        membership: SelectionMembership::EntryPath {
                            head: live
                                .reducer
                                .resolve_incremental(100_000)
                                .reduction
                                .histories[&agent()]
                                .archive_order
                                .back()
                                .unwrap()
                                .clone(),
                        },
                        content_scope: HistoryContentScope::AllObservedAtCut,
                        excluded_heads: vec![],
                    }),
                }
            }
            "activation" => {
                for claim in &mut pending {
                    if let Fact::Declare {
                        object: TraceObject::Message(message),
                    } = &mut claim.fact
                    {
                        message.production.as_mut().unwrap().activation = Some(
                            ActivationRef::native(agent(), "activation", vec!["another".into()])
                                .unwrap(),
                        );
                    }
                }
            }
            "correction" => {
                let old = live.reducer.resolve_incremental(100_000).reduction.messages[&message()]
                    .selected
                    .known()
                    .unwrap()
                    .clone();
                for claim in &mut pending {
                    if let Fact::Declare {
                        object: TraceObject::MessageRevision(revision),
                    } = &mut claim.fact
                    {
                        revision.form = RevisionForm::Correction {
                            replaces: old.clone(),
                        };
                    }
                }
            }
            "sibling" => {
                let root = live
                    .reducer
                    .resolve_incremental(100_000)
                    .reduction
                    .histories[&agent()]
                    .archive_order
                    .front()
                    .unwrap()
                    .clone();
                for claim in &mut pending {
                    match &mut claim.fact {
                        Fact::Declare {
                            object: TraceObject::HistoryEntry(entry),
                        } => entry.predecessor = Some(root.clone()),
                        Fact::Relation {
                            relation: Relation::HistoryPredecessor { predecessor, .. },
                        } => *predecessor = root.clone(),
                        _ => {}
                    }
                }
            }
            "multiple" => {
                pending.extend(live.map(item("item/completed", "other-0", "changed", true)))
            }
            _ => unreachable!(),
        }
        if matches!(guard, "future-object" | "future-claim" | "selection") {
            apply(&mut live.reducer, &[barrier]);
            live.reducer.resolve_incremental(100_000);
        }
        let before = live.reducer.resolve_incremental(100_000).reduction;
        apply(&mut live.reducer, &pending);
        let result = live.reducer.resolve_incremental(100_000);
        assert!(
            result.message_update.is_none(),
            "{guard} requires reconciliation"
        );
        assert_eq!(result.work.full_resolutions, 1, "{guard}");
        assert_eq!(*result.reduction, live.reducer.resolve(), "{guard}");
        assert_eq!(
            result.changes,
            ReductionChanges::between(&before, &result.reduction),
            "{guard}"
        );
    }
}
