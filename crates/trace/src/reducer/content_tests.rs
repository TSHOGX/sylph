use super::*;

fn owner(name: &str) -> AgentRef {
    AgentRef::Session(sylph_protocol::SessionId::decode(&format!("env7:codex:{name}")).unwrap())
}

fn entry(name: &str) -> EntryRef {
    EntryRef::native(owner("root"), "fixture.entry", vec![name.into()]).unwrap()
}

fn message(name: &str) -> MessageRef {
    MessageRef::native(owner("root"), "fixture.message", vec![name.into()]).unwrap()
}

fn revision(name: &str) -> MessageRevisionRef {
    MessageRevisionRef::native(owner("root"), "fixture.revision", vec![name.into()]).unwrap()
}

fn selection(name: &str) -> SelectionRef {
    SelectionRef::native(owner(name), "fixture.selection", vec!["selected".into()]).unwrap()
}

fn position(source: &str, ordinal: u64) -> SourcePosition {
    SourcePosition {
        source: SourceRef {
            issuer: Issuer::Server("fixture".into()),
            instance: source.into(),
        },
        generation: 4,
        locator: SourceLocator::Jsonl {
            byte_start: ordinal * 100,
            byte_end: (ordinal + 1) * 100,
            record_ordinal: ordinal,
        },
    }
}

fn extent(source: &str) -> SourceExtent {
    SourceExtent {
        source: position(source, 7).source,
        generation: 4,
        coordinate: HistoryCoordinate::LocalRecordIndex,
        start_inclusive: 7,
        end_exclusive: 8,
        mapping_rule: "fixture/native-history".into(),
        original_owner: None,
    }
}

#[derive(Clone, Default)]
struct Fixture {
    claims: Vec<Claim>,
}

impl Fixture {
    fn fact(&mut self, fact: Fact) -> ClaimRef {
        let ordinal = self.claims.len() as u64 + 1;
        let reference = ClaimRef::occurrence(
            owner("root"),
            position("claims", ordinal),
            vec!["fact".into()],
        )
        .unwrap();
        self.claims.push(Claim {
            claim_ref: reference.clone(),
            fact,
            evidence: vec![ObservationRef {
                issuer: Issuer::Server("fixture".into()),
                producer_id: "retained".into(),
                producer_epoch: "one".into(),
                producer_seq: ordinal,
            }],
            rule_id: "fixture/explicit-content-evidence".into(),
            mapper_build: "test".into(),
            supersedes: vec![],
            source_revision: None,
        });
        reference
    }

    fn declare(&mut self, object: TraceObject) {
        self.fact(Fact::Declare { object });
    }

    fn entry(&mut self, name: &str, predecessor: Option<&str>) {
        self.declare(TraceObject::HistoryEntry(HistoryEntry {
            entry_ref: entry(name),
            containing_agent: owner("root"),
            predecessor: predecessor.map(entry),
            original_owner: None,
        }));
    }

    fn message(&mut self, name: &str) {
        self.declare(TraceObject::Message(Message {
            message_ref: message(name),
            author: Author::Agent {
                agent: OwnerRef::Agent {
                    agent: owner("root"),
                },
            },
            protocol_role: Some(ProtocolRole::Assistant),
            purpose: MessagePurpose::Answer,
            production: Some(ProductionScope {
                owner: OwnerRef::Agent {
                    agent: owner("root"),
                },
                activation: None,
            }),
        }));
    }

    fn revision(&mut self, name: &str, message_id: &str, digest: char) {
        self.declare(TraceObject::MessageRevision(MessageRevision {
            revision_ref: revision(name),
            message: message(message_id),
            representation: Representation::Conversation,
            form: RevisionForm::Complete,
            parts: vec![ContentPart {
                part_ref: MessagePartRef::native(
                    owner("root"),
                    "fixture.part",
                    vec![name.into(), "0".into()],
                )
                .unwrap(),
                kind: ContentKind::Text,
                payload: PayloadRef::bytes(digest.to_string().repeat(64)).unwrap(),
                media_type: Some("text/plain".into()),
                encoding: PayloadEncoding::Utf8,
            }],
        }));
    }

    fn content(
        &mut self,
        name: &str,
        position: SourcePosition,
        rule: &str,
        revisions: &[&str],
        complete: bool,
    ) -> ClaimRef {
        self.fact(Fact::HistoryContentObserved {
            observation: HistoryContentObservation {
                entry: entry(name),
                position,
                mapping_rule: rule.into(),
                slots: revisions
                    .iter()
                    .enumerate()
                    .map(|(slot, name)| HistorySlot {
                        slot: slot as u32,
                        content: HistoryContent::Message(revision(name)),
                    })
                    .collect(),
                complete,
            },
        })
    }

    fn mapped_position(&mut self, name: &str, source: &str) {
        self.fact(Fact::HistoryPosition {
            entry: entry(name),
            position: position(source, 7),
            coordinate: HistoryCoordinate::LocalRecordIndex,
            value: 7,
            mapping_rule: "fixture/native-history".into(),
        });
        self.fact(Fact::HistoryMappingComplete {
            extent: extent(source),
        });
    }

    fn select(&mut self, name: &str, membership: SelectionMembership) {
        self.declare(TraceObject::HistorySelection(HistorySelection {
            selection_ref: selection(name),
            agent: owner(name),
            membership,
            content_scope: HistoryContentScope::AllObservedAtCut,
            excluded_heads: vec![],
        }));
        self.fact(Fact::SelectHistory {
            agent: owner(name),
            selection: selection(name),
            selected_at: position("selection", self.claims.len() as u64 + 1),
        });
    }

    fn reduce(&self) -> Reduction {
        let evidence = self
            .claims
            .iter()
            .flat_map(|claim| claim.evidence.clone())
            .collect();
        let mut reducer = Reducer::default();
        reducer.apply(&self.claims, &evidence, 1000).unwrap();
        reducer.resolve()
    }
}

fn two_revisions(second_digest: char) -> Fixture {
    let mut fixture = Fixture::default();
    fixture.entry("before", None);
    fixture.entry("answer", Some("before"));
    fixture.message("answer");
    fixture.revision("native", "answer", 'a');
    fixture.revision("rpc", "answer", second_digest);
    fixture.content(
        "answer",
        position("native", 7),
        "fixture/native-history",
        &["native"],
        true,
    );
    fixture.content(
        "answer",
        position("rpc", 19),
        "fixture/rpc-history",
        &["rpc"],
        true,
    );
    fixture.select(
        "root",
        SelectionMembership::EntryPath {
            head: entry("answer"),
        },
    );
    fixture
}

#[test]
fn one_delivery_keeps_observed_revision_bindings_and_respects_selected_content() {
    let mut fixture = two_revisions('a');
    let delivery = DeliveryRef::native(owner("root"), "admission", vec!["entry".into()]).unwrap();
    fixture.declare(TraceObject::Delivery(Delivery {
        delivery_ref: delivery.clone(),
        sender: Author::Unknown,
        recipient: Recipient::Agent {
            agent: OwnerRef::Agent {
                agent: owner("root"),
            },
        },
        route: DeliveryRoute::NativeHistory,
    }));
    for name in ["native", "rpc"] {
        fixture.fact(Fact::DeliveryRevisionObserved {
            delivery: delivery.clone(),
            revision: revision(name),
        });
    }
    let reduced = fixture.reduce();
    assert_eq!(reduced.delivery_revisions[&delivery].len(), 2);
    assert!(reduced
        .select_delivery_revision(&delivery, None)
        .known()
        .is_some());
    assert_eq!(
        reduced
            .select_delivery_revision(&delivery, Some(&BTreeSet::from([revision("native")])))
            .known(),
        Some(&revision("native"))
    );
    assert!(reduced
        .select_delivery_revision(&delivery, Some(&BTreeSet::new()))
        .known()
        .is_none());
    assert!(reduced
        .object(&AnyObjectRef::Delivery(delivery))
        .unwrap()
        .declaration
        .known()
        .is_some());
}

#[test]
fn correcting_message_content_does_not_choose_a_different_observed_delivery_body() {
    let mut fixture = two_revisions('b');
    for claim in &mut fixture.claims {
        if let Fact::Declare {
            object: TraceObject::MessageRevision(value),
        } = &mut claim.fact
        {
            if value.revision_ref == revision("rpc") {
                value.form = RevisionForm::Correction {
                    replaces: revision("native"),
                };
            }
        }
    }
    let delivery = DeliveryRef::native(owner("root"), "admission", vec!["entry".into()]).unwrap();
    for name in ["native", "rpc"] {
        fixture.fact(Fact::DeliveryRevisionObserved {
            delivery: delivery.clone(),
            revision: revision(name),
        });
    }
    let reduced = fixture.reduce();
    let revisions = BTreeSet::from([revision("native"), revision("rpc")]);
    assert_eq!(
        reduced
            .select_message_in(&message("answer"), &revisions)
            .known(),
        Some(&revision("rpc"))
    );
    assert!(matches!(
        reduced.select_delivery_revision(&delivery, None),
        Resolved::Conflicted { .. }
    ));
}

#[test]
fn retired_content_bearing_identity_shapes_are_rejected() {
    let topology = HistoryEntry {
        entry_ref: entry("answer"),
        containing_agent: owner("root"),
        predecessor: None,
        original_owner: None,
    };
    let mut old_entry = serde_json::to_value(topology).unwrap();
    old_entry["content"] = serde_json::json!([]);
    assert!(serde_json::from_value::<HistoryEntry>(old_entry).is_err());
    let delivery = Delivery {
        delivery_ref: DeliveryRef::native(owner("root"), "admission", vec!["entry".into()])
            .unwrap(),
        sender: Author::Unknown,
        recipient: Recipient::Unknown,
        route: DeliveryRoute::NativeHistory,
    };
    let mut old_delivery = serde_json::to_value(delivery).unwrap();
    old_delivery["messageRevision"] = serde_json::to_value(revision("native")).unwrap();
    assert!(serde_json::from_value::<Delivery>(old_delivery).is_err());
}

#[test]
fn identical_manifest_handles_without_completeness_evidence_do_not_establish_complete_content() {
    let mut fixture = two_revisions('a');
    for claim in &mut fixture.claims {
        if let Fact::Declare {
            object: TraceObject::MessageRevision(revision),
        } = &mut claim.fact
        {
            revision.parts[0].payload = PayloadRef::manifest("a".repeat(64)).unwrap();
        }
    }
    let reduced = fixture.reduce();
    assert!(matches!(
        reduced.select_message_in(
            &message("answer"),
            &BTreeSet::from([revision("native"), revision("rpc")])
        ),
        Resolved::Conflicted { .. }
    ));
}

fn assert_topology(reduced: &Reduction) {
    let Some(TraceObject::HistoryEntry(value)) = reduced
        .object(&AnyObjectRef::HistoryEntry(entry("answer")))
        .and_then(|object| object.declaration.known())
    else {
        panic!("Content alternatives must not invalidate the native entry")
    };
    assert_eq!(value.predecessor, Some(entry("before")));
    assert_eq!(
        reduced.histories[&owner("root")].selected_entries,
        Some(imbl::vector![entry("before"), entry("answer")])
    );
}

#[test]
fn equal_complete_presentations_keep_both_revision_identities_and_proven_topology() {
    let reduced = two_revisions('a').reduce();
    assert_topology(&reduced);
    let contents = reduced.entry_contents(&entry("answer"), Some(&selection("root")));
    assert!(contents.complete);
    assert_eq!(
        contents.slots[&0].known(),
        Some(&vec![
            HistoryContent::Message(revision("native")),
            HistoryContent::Message(revision("rpc"))
        ])
    );
    let selected = reduced.select_message_in(
        &message("answer"),
        &BTreeSet::from([revision("native"), revision("rpc")]),
    );
    assert_eq!(selected.known(), Some(&revision("native")));
    assert_eq!(reduced.messages[&message("answer")].revisions.len(), 2);
    for name in ["native", "rpc"] {
        let reference = AnyObjectRef::MessageRevision(revision(name));
        assert_eq!(reduced.object(&reference).unwrap().reference, reference);
    }
}

#[test]
fn disagreeing_complete_bodies_conflict_without_erasing_the_entry_or_its_predecessor() {
    let reduced = two_revisions('b').reduce();
    assert_topology(&reduced);
    let contents = reduced.entry_contents(&entry("answer"), Some(&selection("root")));
    assert!(contents.complete);
    assert_eq!(contents.slots[&0].known().unwrap().len(), 2);
    assert!(matches!(
        reduced.select_message_in(&message("answer"), &BTreeSet::from([revision("native"), revision("rpc")])),
        Resolved::Conflicted { alternatives } if alternatives.len() == 2
    ));
    assert_eq!(
        reduced
            .select_message_in(&message("answer"), &BTreeSet::from([revision("native")]))
            .known(),
        Some(&revision("native"))
    );
}

#[test]
fn source_extent_content_requires_the_exact_position_generation_source_and_mapping_rule() {
    let mut fixture = Fixture::default();
    fixture.entry("answer", None);
    fixture.message("answer");
    fixture.revision("native", "answer", 'a');
    fixture.revision("other", "answer", 'b');
    fixture.mapped_position("answer", "native");
    let included = fixture.content(
        "answer",
        position("native", 7),
        "fixture/native-history",
        &["native"],
        true,
    );
    fixture.select(
        "root",
        SelectionMembership::SourceExtents {
            extents: vec![extent("native")],
            inherited_selections: vec![],
        },
    );
    let mut other_generation = position("native", 7);
    other_generation.generation = 5;
    for (label, position, rule) in [
        ("source", position("rpc", 7), "fixture/native-history"),
        ("generation", other_generation, "fixture/native-history"),
        ("position", position("native", 8), "fixture/native-history"),
        (
            "mapping rule",
            position("native", 7),
            "fixture/other-history",
        ),
    ] {
        let mut variant = fixture.clone();
        variant.content("answer", position, rule, &["other"], true);
        let reduced = variant.reduce();
        assert_eq!(
            reduced.selection_memberships[&selection("root")],
            Some(vec![entry("answer")]),
            "{label}"
        );
        assert_eq!(
            reduced.selection_content_observations[&selection("root")],
            Some(BTreeSet::from([included.clone()])),
            "{label}"
        );
        assert_eq!(
            reduced
                .entry_contents(&entry("answer"), Some(&selection("root")))
                .slots[&0]
                .known(),
            Some(&vec![HistoryContent::Message(revision("native"))]),
            "{label}"
        );
        assert_eq!(
            reduced.entry_contents(&entry("answer"), None).slots[&0]
                .known()
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn inherited_selections_preserve_the_original_content_bindings_after_later_observations() {
    let mut fixture = Fixture::default();
    fixture.entry("answer", None);
    fixture.message("answer");
    fixture.revision("native", "answer", 'a');
    fixture.revision("later", "answer", 'b');
    fixture.mapped_position("answer", "native");
    let included = fixture.content(
        "answer",
        position("native", 7),
        "fixture/native-history",
        &["native"],
        true,
    );
    fixture.select(
        "root",
        SelectionMembership::SourceExtents {
            extents: vec![extent("native")],
            inherited_selections: vec![],
        },
    );
    fixture.select(
        "child",
        SelectionMembership::SourceExtents {
            extents: vec![],
            inherited_selections: vec![selection("root")],
        },
    );
    fixture.select(
        "grandchild",
        SelectionMembership::SourceExtents {
            extents: vec![],
            inherited_selections: vec![selection("child")],
        },
    );
    let before = fixture.reduce();
    fixture.content(
        "answer",
        position("rpc", 19),
        "fixture/rpc-history",
        &["later"],
        true,
    );
    let after = fixture.reduce();
    for name in ["root", "child", "grandchild"] {
        assert_eq!(
            after.selection_memberships[&selection(name)],
            Some(vec![entry("answer")])
        );
        assert_eq!(
            after.selection_content_observations[&selection(name)],
            Some(BTreeSet::from([included.clone()]))
        );
        assert_eq!(
            after.entry_contents(&entry("answer"), Some(&selection(name))),
            before.entry_contents(&entry("answer"), Some(&selection(name)))
        );
        assert_eq!(
            after
                .entry_contents(&entry("answer"), Some(&selection(name)))
                .slots[&0]
                .known(),
            Some(&vec![HistoryContent::Message(revision("native"))])
        );
    }
    assert_eq!(
        before.entry_contents(&entry("answer"), None).slots[&0]
            .known()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        after.entry_contents(&entry("answer"), None).slots[&0]
            .known()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn complete_empty_content_is_distinct_from_absent_and_partial_observations() {
    let mut fixture = Fixture::default();
    for name in [
        "absent",
        "partial-empty",
        "complete-empty",
        "partial-content",
    ] {
        fixture.entry(name, None);
    }
    fixture.message("answer");
    fixture.revision("native", "answer", 'a');
    fixture.content(
        "partial-empty",
        position("native", 1),
        "fixture/native-history",
        &[],
        false,
    );
    fixture.content(
        "complete-empty",
        position("native", 2),
        "fixture/native-history",
        &[],
        true,
    );
    fixture.content(
        "partial-content",
        position("native", 3),
        "fixture/native-history",
        &["native"],
        false,
    );
    let reduced = fixture.reduce();
    for name in ["absent", "partial-empty"] {
        let contents = reduced.entry_contents(&entry(name), None);
        assert!(
            !contents.complete,
            "{name} must not claim observed emptiness"
        );
        assert!(contents.slots.is_empty());
    }
    let empty = reduced.entry_contents(&entry("complete-empty"), None);
    assert!(empty.complete);
    assert!(empty.slots.is_empty());
    let partial = reduced.entry_contents(&entry("partial-content"), None);
    assert!(!partial.complete);
    assert_eq!(
        partial.slots[&0].known(),
        Some(&vec![HistoryContent::Message(revision("native"))])
    );
}

#[test]
fn complete_empty_observation_disagrees_with_a_present_slot_without_erasing_topology() {
    let mut fixture = Fixture::default();
    fixture.entry("answer", None);
    fixture.message("answer");
    fixture.revision("native", "answer", 'a');
    fixture.content(
        "answer",
        position("native", 7),
        "fixture/native-history",
        &["native"],
        false,
    );
    fixture.content(
        "answer",
        position("rpc", 19),
        "fixture/rpc-history",
        &[],
        true,
    );
    let reduced = fixture.reduce();
    assert!(reduced
        .object(&AnyObjectRef::HistoryEntry(entry("answer")))
        .unwrap()
        .declaration
        .known()
        .is_some());
    let contents = reduced.entry_contents(&entry("answer"), None);
    assert!(contents.complete);
    let Resolved::Conflicted { alternatives } = &contents.slots[&0] else {
        panic!("Observed absence cannot erase an independently observed slot")
    };
    assert!(alternatives.iter().any(|(contents, _)| contents.is_empty()));
    assert!(alternatives
        .iter()
        .any(|(contents, _)| contents == &vec![HistoryContent::Message(revision("native"))]));
}

#[test]
fn equal_payloads_in_distinct_messages_cannot_merge_a_slot_or_message_identity() {
    let mut fixture = Fixture::default();
    fixture.entry("answer", None);
    fixture.entry("two-slots", None);
    for name in ["first", "second"] {
        fixture.message(name);
        fixture.revision(name, name, 'a');
    }
    fixture.content(
        "answer",
        position("native", 7),
        "fixture/native-history",
        &["first"],
        true,
    );
    fixture.content(
        "answer",
        position("rpc", 19),
        "fixture/rpc-history",
        &["second"],
        true,
    );
    fixture.content(
        "two-slots",
        position("native", 8),
        "fixture/native-history",
        &["first", "second"],
        true,
    );
    let reduced = fixture.reduce();
    let contents = reduced.entry_contents(&entry("answer"), None);
    assert!(
        matches!(&contents.slots[&0], Resolved::Conflicted { alternatives } if alternatives.len() == 2)
    );
    for name in ["first", "second"] {
        let reference = AnyObjectRef::Message(message(name));
        assert_eq!(reduced.object(&reference).unwrap().reference, reference);
    }
    let repeated = reduced.entry_contents(&entry("two-slots"), None);
    assert_eq!(repeated.slots.len(), 2);
    assert_eq!(
        repeated.slots[&0].known(),
        Some(&vec![HistoryContent::Message(revision("first"))])
    );
    assert_eq!(
        repeated.slots[&1].known(),
        Some(&vec![HistoryContent::Message(revision("second"))])
    );
}
