use super::*;

fn owner(name: &str) -> AgentRef {
    AgentRef::Session(sylph_protocol::SessionId::decode(&format!("env7:codex:{name}")).unwrap())
}
fn source() -> SourceRef {
    SourceRef {
        issuer: Issuer::Server("fixture".into()),
        instance: "native".into(),
    }
}
fn pos(ordinal: u64) -> SourcePosition {
    SourcePosition {
        source: source(),
        generation: 0,
        locator: SourceLocator::Frame { ordinal },
    }
}
fn entry(id: &str) -> EntryRef {
    EntryRef::native(owner("parent"), "entries", vec![id.into()]).unwrap()
}
fn selection(id: &str) -> SelectionRef {
    SelectionRef::native(owner(id), "selection", vec!["selected".into()]).unwrap()
}
fn fact(index: u64, fact: Fact) -> Claim {
    Claim {
        claim_ref: ClaimRef::occurrence(owner("parent"), pos(index), vec!["claim".into()]).unwrap(),
        fact,
        evidence: vec![ObservationRef {
            issuer: Issuer::Server("fixture".into()),
            producer_id: "source".into(),
            producer_epoch: "epoch".into(),
            producer_seq: index,
        }],
        rule_id: "fixture".into(),
        mapper_build: "test".into(),
        supersedes: vec![],
        source_revision: None,
    }
}
fn entry_fact(index: u64, id: &str, parent: Option<&str>) -> Claim {
    fact(
        index,
        Fact::Declare {
            object: TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry(id),
                containing_agent: owner("parent"),
                content: vec![],
                predecessor: parent.map(entry),
                original_owner: None,
            }),
        },
    )
}
fn selection_facts(index: u64, name: &str, membership: SelectionMembership) -> [Claim; 2] {
    [
        fact(
            index,
            Fact::Declare {
                object: TraceObject::HistorySelection(HistorySelection {
                    selection_ref: selection(name),
                    agent: owner(name),
                    membership,
                    excluded_heads: vec![],
                }),
            },
        ),
        fact(
            index + 1,
            Fact::SelectHistory {
                agent: owner(name),
                selection: selection(name),
                selected_at: pos(index + 1),
            },
        ),
    ]
}
fn reduce_history(claims: &[Claim]) -> Reduction {
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.clone())
        .collect();
    let mut reducer = Reducer::default();
    reducer.apply(claims, &evidence, 1000).unwrap();
    reducer.resolve()
}
fn extent(start: u64, end: u64) -> SourceExtent {
    SourceExtent {
        source: source(),
        generation: 0,
        coordinate: HistoryCoordinate::VendorOrdinal {
            domain_ref: source(),
        },
        start_inclusive: start,
        end_exclusive: end,
        mapping_rule: "native/history".into(),
        original_owner: None,
    }
}
fn inherited_fixture(conflicting: bool) -> Vec<Claim> {
    let mut claims = vec![
        entry_fact(1, "a", None),
        entry_fact(2, "b", None),
        entry_fact(3, "c", if conflicting { Some("a") } else { None }),
        fact(
            4,
            Fact::Relation {
                relation: Relation::HistoryPredecessor {
                    predecessor: entry(if conflicting { "b" } else { "a" }),
                    entry: entry("c"),
                },
            },
        ),
    ];
    claims.extend(selection_facts(
        5,
        "parent",
        SelectionMembership::EntryPath { head: entry("c") },
    ));
    claims.extend(selection_facts(
        7,
        "child",
        SelectionMembership::SourceExtents {
            extents: vec![],
            inherited_selections: vec![selection("parent")],
        },
    ));
    claims
}

#[test]
fn inherited_paths_use_the_same_evidenced_predecessor_as_direct_reads() {
    let result = reduce_history(&inherited_fixture(false));
    assert_eq!(
        result.histories[&owner("parent")].selected_entries,
        Some(vec![entry("a"), entry("c")])
    );
    assert_eq!(
        result.histories[&owner("child")].selected_entries,
        Some(vec![entry("a"), entry("c")])
    );
}

#[test]
fn inherited_paths_preserve_direct_predecessor_conflicts() {
    let result = reduce_history(&inherited_fixture(true));
    assert!(result.histories[&owner("parent")]
        .selected_entries
        .is_none());
    assert!(result.histories[&owner("child")].selected_entries.is_none());
}

#[test]
fn a_missing_source_extent_is_unknown_until_empty_mapping_is_proved() {
    let range = extent(214, 215);
    let mut claims = selection_facts(
        1,
        "child",
        SelectionMembership::SourceExtents {
            extents: vec![range.clone()],
            inherited_selections: vec![],
        },
    )
    .to_vec();
    assert!(reduce_history(&claims).histories[&owner("child")]
        .selected_entries
        .is_none());
    claims.push(fact(3, Fact::HistoryMappingComplete { extent: range }));
    assert_eq!(
        reduce_history(&claims).histories[&owner("child")].selected_entries,
        Some(vec![])
    );
}

#[test]
fn mapping_witnesses_must_cover_every_requested_coordinate() {
    let mut claims = selection_facts(
        1,
        "child",
        SelectionMembership::SourceExtents {
            extents: vec![extent(214, 217)],
            inherited_selections: vec![],
        },
    )
    .to_vec();
    claims.push(fact(
        3,
        Fact::HistoryMappingComplete {
            extent: extent(214, 215),
        },
    ));
    claims.push(fact(
        4,
        Fact::HistoryMappingComplete {
            extent: extent(216, 217),
        },
    ));
    assert!(reduce_history(&claims).histories[&owner("child")]
        .selected_entries
        .is_none());
    claims.push(fact(
        5,
        Fact::HistoryMappingComplete {
            extent: extent(215, 216),
        },
    ));
    assert_eq!(
        reduce_history(&claims).histories[&owner("child")].selected_entries,
        Some(vec![])
    );
}

#[test]
fn conflicting_positions_cannot_choose_membership_by_one_convenient_value() {
    let mut claims = vec![
        entry_fact(1, "a", None),
        fact(
            2,
            Fact::HistoryPosition {
                entry: entry("a"),
                position: pos(1),
                coordinate: extent(214, 215).coordinate,
                value: 214,
                mapping_rule: "native/history".into(),
            },
        ),
        fact(
            3,
            Fact::HistoryPosition {
                entry: entry("a"),
                position: pos(1),
                coordinate: extent(214, 215).coordinate,
                value: 999,
                mapping_rule: "native/history".into(),
            },
        ),
        fact(
            4,
            Fact::HistoryMappingComplete {
                extent: extent(214, 215),
            },
        ),
    ];
    claims.extend(selection_facts(
        5,
        "child",
        SelectionMembership::SourceExtents {
            extents: vec![extent(214, 215)],
            inherited_selections: vec![],
        },
    ));
    assert!(reduce_history(&claims).histories[&owner("child")]
        .selected_entries
        .is_none());
}

#[test]
fn invalid_mapping_witness_ranges_are_rejected_before_reduction() {
    let invalid = fact(
        1,
        Fact::HistoryMappingComplete {
            extent: extent(215, 214),
        },
    );
    assert!(invalid.validate().is_err());
}

#[test]
fn presentation_ties_never_claim_native_or_explicit_causality() {
    let tied = reduce_history(&[entry_fact(1, "a", None), entry_fact(2, "b", None)]);
    let view = &tied.histories[&owner("parent")];
    assert_eq!(
        view.order_basis.get(&entry("a")),
        Some(&ReadingOrderBasis::PresentationTiebreak)
    );
    assert_eq!(
        view.order_basis.get(&entry("b")),
        Some(&ReadingOrderBasis::PresentationTiebreak)
    );
    let native = reduce_history(&[entry_fact(1, "a", None), entry_fact(2, "b", Some("a"))]);
    assert_eq!(
        native.histories[&owner("parent")]
            .order_basis
            .get(&entry("b")),
        Some(&ReadingOrderBasis::NativePredecessor)
    );
    let explicit = reduce_history(&[
        entry_fact(1, "a", None),
        entry_fact(2, "b", None),
        fact(
            3,
            Fact::Relation {
                relation: Relation::ReadingOrder {
                    before: entry("b"),
                    after: entry("a"),
                },
            },
        ),
    ]);
    assert_eq!(
        explicit.histories[&owner("parent")]
            .order_basis
            .get(&entry("a")),
        Some(&ReadingOrderBasis::ExplicitOrder)
    );
}
