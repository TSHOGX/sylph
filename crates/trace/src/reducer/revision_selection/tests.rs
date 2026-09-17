use super::super::*;
use super::{comparison_work, reset_comparison_work};
use std::cmp::Ordering;

fn agent() -> AgentRef {
    AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:selection").unwrap())
}

fn revision(index: usize) -> MessageRevisionRef {
    MessageRevisionRef::native(agent(), "revision", vec![index.to_string()]).unwrap()
}

fn position(source: usize, generation: u64, locator: SourceLocator) -> SourcePosition {
    SourcePosition {
        source: SourceRef {
            issuer: Issuer::Server("selection".into()),
            instance: source.to_string(),
        },
        generation,
        locator,
    }
}

fn candidate(
    index: usize,
    value: usize,
    form: RevisionForm,
    at: Option<SourcePosition>,
) -> RevisionCandidate {
    let claim = match &at {
        Some(at) => ClaimRef::occurrence(agent(), at.clone(), vec![index.to_string()]).unwrap(),
        None => ClaimRef::native(agent(), "support", vec![index.to_string()]).unwrap(),
    };
    RevisionCandidate {
        reference: revision(value),
        form,
        position: at,
        claim,
    }
}

// Keep the pairwise definition independent of the indexed implementation. Exact
// alternative and support order matters as well as the selected reference.
fn pair_order(a: &SourcePosition, b: &SourcePosition) -> Option<Ordering> {
    if a.source != b.source || a.generation != b.generation {
        return None;
    }
    match (&a.locator, &b.locator) {
        (
            SourceLocator::Jsonl { byte_start: a, .. },
            SourceLocator::Jsonl { byte_start: b, .. },
        ) => Some(a.cmp(b)),
        (SourceLocator::Frame { ordinal: a }, SourceLocator::Frame { ordinal: b }) => {
            Some(a.cmp(b))
        }
        (a, b) if a == b => Some(Ordering::Equal),
        _ => None,
    }
}

fn seals<'a>(values: &[&'a RevisionCandidate]) -> Vec<&'a RevisionCandidate> {
    let replaced: BTreeSet<_> = values
        .iter()
        .filter_map(|value| match &value.form {
            RevisionForm::Correction { replaces } => Some(replaces),
            _ => None,
        })
        .collect();
    values
        .iter()
        .copied()
        .filter(|value| {
            !replaced.contains(&value.reference)
                && matches!(
                    value.form,
                    RevisionForm::Complete
                        | RevisionForm::Final { .. }
                        | RevisionForm::Correction { .. }
                )
        })
        .collect()
}

fn oracle(values: &[&RevisionCandidate]) -> Resolved<MessageRevisionRef> {
    let finals = seals(values);
    let selected = if finals.is_empty() {
        values
            .iter()
            .copied()
            .filter(|a| {
                !values.iter().any(|b| {
                    a.position
                        .as_ref()
                        .zip(b.position.as_ref())
                        .is_some_and(|(a, b)| pair_order(a, b) == Some(Ordering::Less))
                })
            })
            .collect()
    } else {
        finals
    };
    let mut alternatives: Vec<(MessageRevisionRef, Vec<ClaimRef>)> = Vec::new();
    for value in selected {
        if let Some((_, support)) = alternatives
            .iter_mut()
            .find(|(reference, _)| reference == &value.reference)
        {
            support.push(value.claim.clone());
        } else {
            alternatives.push((value.reference.clone(), vec![value.claim.clone()]));
        }
    }
    match alternatives.len() {
        0 => Resolved::Unknown,
        1 => {
            let (value, claims) = alternatives.pop().unwrap();
            Resolved::Known { value, claims }
        }
        _ => Resolved::Conflicted { alternatives },
    }
}

fn history(
    values: &[RevisionCandidate],
    conflicted: &BTreeSet<MessageRevisionRef>,
) -> MessageHistory {
    let message = MessageRef::native(agent(), "message", vec!["stream".into()]).unwrap();
    let claims: Vec<_> = values
        .iter()
        .map(|value| Claim {
            claim_ref: value.claim.clone(),
            fact: Fact::Declare {
                object: TraceObject::MessageRevision(MessageRevision {
                    revision_ref: value.reference.clone(),
                    message: message.clone(),
                    representation: Representation::Conversation,
                    form: value.form.clone(),
                    parts: vec![],
                }),
            },
            evidence: vec![],
            rule_id: "selection-oracle".into(),
            mapper_build: "test".into(),
            supersedes: vec![],
            source_revision: None,
        })
        .collect();
    let mut reduction = Reduction::default();
    for reference in conflicted {
        let object = AnyObjectRef::MessageRevision(reference.clone());
        reduction.objects.insert(
            object.clone(),
            ResolvedObject {
                reference: object,
                declaration: Resolved::Conflicted {
                    alternatives: vec![],
                },
            },
        );
    }
    resolve_messages(&claims.iter().collect::<Vec<_>>(), &mut reduction);
    reduction.messages.remove(&message).unwrap()
}

#[test]
fn source_order_preserves_all_locator_domain_and_equal_start_rules() {
    let locators = [
        SourceLocator::Frame { ordinal: 0 },
        SourceLocator::Frame { ordinal: u64::MAX },
        SourceLocator::Jsonl {
            byte_start: 1,
            byte_end: 2,
            record_ordinal: 9,
        },
        SourceLocator::Jsonl {
            byte_start: 1,
            byte_end: 9,
            record_ordinal: 0,
        },
        SourceLocator::Jsonl {
            byte_start: 2,
            byte_end: 3,
            record_ordinal: 1,
        },
        SourceLocator::Snapshot {
            revision: "10".into(),
        },
        SourceLocator::Snapshot {
            revision: "2".into(),
        },
        SourceLocator::Occurrence { key: "10".into() },
        SourceLocator::Occurrence { key: "2".into() },
    ];
    for source_a in 0..2 {
        for source_b in 0..2 {
            for generation_a in 0..2 {
                for generation_b in 0..2 {
                    for a in &locators {
                        for b in &locators {
                            let a = position(source_a, generation_a, a.clone());
                            let b = position(source_b, generation_b, b.clone());
                            assert_eq!(source_order(&a, &b), pair_order(&a, &b), "{a:?} / {b:?}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn indexed_selection_preserves_pairwise_values_and_support_order_under_mixed_inputs() {
    for seed in 0..96usize {
        let mut values: Vec<_> = (0..12)
            .map(|i| {
                let n = i + seed;
                let at = match n % 5 {
                    0 => None,
                    1 => Some(position(
                        n % 3,
                        (n % 2) as u64,
                        SourceLocator::Frame {
                            ordinal: (i % 4) as u64,
                        },
                    )),
                    2 => Some(position(
                        n % 3,
                        (n % 2) as u64,
                        SourceLocator::Jsonl {
                            byte_start: (i % 4) as u64,
                            byte_end: (i % 4 + 10) as u64,
                            record_ordinal: i as u64,
                        },
                    )),
                    3 => Some(position(
                        n % 3,
                        0,
                        SourceLocator::Snapshot {
                            revision: (i % 4).to_string(),
                        },
                    )),
                    _ => Some(position(
                        n % 3,
                        0,
                        SourceLocator::Occurrence {
                            key: (i % 4).to_string(),
                        },
                    )),
                };
                let form = match (i * 5 + seed) % 7 {
                    0 => RevisionForm::Complete,
                    1 => RevisionForm::Final { covers: None },
                    2 => RevisionForm::Correction {
                        replaces: revision((i + seed + 1) % 7),
                    },
                    3 => RevisionForm::Replacement { covers: None },
                    _ => RevisionForm::Append { predecessor: None },
                };
                candidate(i, (i * 3 + seed) % 7, form, at)
            })
            .collect();
        for rotation in 0..12 {
            values.rotate_left(1);
            if rotation % 2 == 0 {
                values.reverse();
            }
            let refs: Vec<_> = values.iter().collect();
            assert_eq!(
                select_revision(&refs),
                oracle(&refs),
                "seed={seed}, rotation={rotation}"
            );
            let unsealed: Vec<_> = refs
                .iter()
                .copied()
                .filter(|value| {
                    matches!(
                        value.form,
                        RevisionForm::Append { .. } | RevisionForm::Replacement { .. }
                    )
                })
                .collect();
            assert_eq!(
                select_revision(&unsealed),
                oracle(&unsealed),
                "unsealed seed={seed}, rotation={rotation}"
            );
            let actual = history(&values, &BTreeSet::new());
            let finals = seals(&refs);
            let expected_late: Vec<_> = values
                .iter()
                .filter(|value| {
                    matches!(value.form, RevisionForm::Append { .. })
                        && finals.iter().any(|seal| {
                            value
                                .position
                                .as_ref()
                                .zip(seal.position.as_ref())
                                .is_some_and(|(a, b)| pair_order(a, b) == Some(Ordering::Greater))
                        })
                })
                .map(|value| value.reference.clone())
                .collect();
            assert_eq!(actual.selected, oracle(&refs));
            assert_eq!(actual.sealed, !finals.is_empty());
            assert_eq!(actual.late_segments, expected_late);
        }
    }
}

#[test]
fn tied_maxima_preserve_interleaved_support_and_incomparable_alternative_order() {
    let frame = |n| Some(position(0, 0, SourceLocator::Frame { ordinal: n }));
    let append = || RevisionForm::Append { predecessor: None };
    let values = vec![
        candidate(0, 0, append(), frame(9)),
        candidate(1, 1, append(), frame(9)),
        candidate(2, 0, append(), frame(9)),
        candidate(3, 2, append(), frame(8)),
        candidate(
            4,
            3,
            append(),
            Some(position(
                0,
                0,
                SourceLocator::Jsonl {
                    byte_start: 1,
                    byte_end: 2,
                    record_ordinal: 80,
                },
            )),
        ),
        candidate(
            5,
            4,
            append(),
            Some(position(
                0,
                0,
                SourceLocator::Jsonl {
                    byte_start: 1,
                    byte_end: 9,
                    record_ordinal: 0,
                },
            )),
        ),
        candidate(
            6,
            5,
            append(),
            Some(position(1, 0, SourceLocator::Frame { ordinal: 1 })),
        ),
        candidate(
            7,
            6,
            append(),
            Some(position(0, 1, SourceLocator::Frame { ordinal: 1 })),
        ),
        candidate(8, 7, append(), None),
    ];
    let selected = select_revision(&values.iter().collect::<Vec<_>>());
    let alternatives = vec![
        (
            revision(0),
            vec![values[0].claim.clone(), values[2].claim.clone()],
        ),
        (revision(1), vec![values[1].claim.clone()]),
        (revision(3), vec![values[4].claim.clone()]),
        (revision(4), vec![values[5].claim.clone()]),
        (revision(5), vec![values[6].claim.clone()]),
        (revision(6), vec![values[7].claim.clone()]),
        (revision(7), vec![values[8].claim.clone()]),
    ];
    assert_eq!(selected, Resolved::Conflicted { alternatives });
}

#[test]
fn any_earlier_surviving_seal_makes_a_same_domain_append_late_without_ordering_finals() {
    let frame = |n| Some(position(0, 0, SourceLocator::Frame { ordinal: n }));
    let values = vec![
        candidate(0, 0, RevisionForm::Final { covers: None }, frame(1)),
        candidate(1, 1, RevisionForm::Final { covers: None }, frame(9)),
        candidate(2, 2, RevisionForm::Append { predecessor: None }, frame(5)),
        candidate(3, 3, RevisionForm::Append { predecessor: None }, frame(1)),
        candidate(
            4,
            4,
            RevisionForm::Append { predecessor: None },
            Some(position(0, 1, SourceLocator::Frame { ordinal: 5 })),
        ),
    ];
    let actual = history(&values, &BTreeSet::new());
    assert_eq!(actual.late_segments, vec![revision(2)]);
    assert_eq!(
        actual.selected,
        Resolved::Conflicted {
            alternatives: vec![
                (revision(0), vec![values[0].claim.clone()]),
                (revision(1), vec![values[1].claim.clone()]),
            ]
        }
    );
}

#[test]
fn correcting_the_earliest_seal_retracts_lateness_and_selection_filters_corrections_first() {
    let at = |ordinal| Some(position(0, 0, SourceLocator::Frame { ordinal }));
    let mut values = vec![
        candidate(0, 0, RevisionForm::Append { predecessor: None }, at(5)),
        candidate(1, 1, RevisionForm::Final { covers: None }, at(1)),
    ];
    assert_eq!(
        history(&values, &BTreeSet::new()).late_segments,
        vec![revision(0)]
    );
    values.push(candidate(
        2,
        2,
        RevisionForm::Correction {
            replaces: revision(1),
        },
        at(9),
    ));
    let reduced = history(&values, &BTreeSet::from([revision(0)]));
    assert_eq!(reduced.selected, Resolved::Unknown);
    assert!(reduced.sealed);
    assert!(reduced.late_segments.is_empty());
    assert_eq!(
        reduced.select_in(&BTreeSet::from([revision(1)])),
        Resolved::Known {
            value: revision(1),
            claims: vec![values[1].claim.clone()],
        }
    );
    assert_eq!(
        reduced.select_in(&BTreeSet::from([revision(1), revision(2)])),
        Resolved::Known {
            value: revision(2),
            claims: vec![values[2].claim.clone()],
        }
    );
    assert_eq!(
        reduced.select_in(&BTreeSet::from([revision(0), revision(2)])),
        Resolved::Unknown
    );
}

#[test]
fn correction_cycles_with_no_surviving_seal_keep_source_maximal_candidates() {
    let values = [
        candidate(
            0,
            0,
            RevisionForm::Correction {
                replaces: revision(1),
            },
            Some(position(0, 0, SourceLocator::Frame { ordinal: 1 })),
        ),
        candidate(
            1,
            1,
            RevisionForm::Correction {
                replaces: revision(0),
            },
            Some(position(0, 0, SourceLocator::Frame { ordinal: 2 })),
        ),
    ];
    let selected = select_revision(&values.iter().collect::<Vec<_>>());
    assert_eq!(
        selected,
        Resolved::Known {
            value: revision(1),
            claims: vec![values[1].claim.clone()]
        }
    );
    assert!(!history(&values, &BTreeSet::new()).sealed);
}

#[test]
fn revision_selection_comparisons_grow_at_most_n_log_n_for_order_and_identity_grouping() {
    for count in [256usize, 4096] {
        for shape in ["ordered", "opaque", "finals", "late"] {
            let values: Vec<_> = (0..count)
                .map(|i| {
                    let at = Some(position(
                        0,
                        0,
                        if shape == "opaque" {
                            SourceLocator::Occurrence { key: i.to_string() }
                        } else {
                            SourceLocator::Frame { ordinal: i as u64 }
                        },
                    ));
                    let form = if shape == "finals" || (shape == "late" && i >= count / 2) {
                        RevisionForm::Final { covers: None }
                    } else {
                        RevisionForm::Append { predecessor: None }
                    };
                    candidate(i, i, form, at)
                })
                .collect();
            reset_comparison_work();
            let actual = history(&values, &BTreeSet::new());
            let work = comparison_work();
            let levels = count.ilog2() as usize + 1;
            let bound = 24 * count * levels;
            eprintln!("revision selection: shape={shape} candidates={count} comparisons={work:?} bound={bound}");
            assert!(
                work.source + work.identity <= bound,
                "{shape}, K={count}: {work:?} exceeds {bound}"
            );
            match shape {
                "ordered" => assert_eq!(actual.selected.known(), Some(&revision(count - 1))),
                "opaque" | "finals" => match actual.selected {
                    Resolved::Conflicted { alternatives } => assert_eq!(alternatives.len(), count),
                    other => panic!("all incomparable candidates remain alternatives: {other:?}"),
                },
                "late" => {
                    assert!(actual.late_segments.is_empty());
                    assert!(actual.sealed);
                }
                _ => unreachable!(),
            }
        }
    }
}
