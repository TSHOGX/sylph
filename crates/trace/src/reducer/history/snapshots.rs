//! Persistently shared snapshot maps retain inactive branches without duplicating
//! every earlier observation in each poll's claim or mapper checkpoint.
use super::*;

type EntryBindings = imbl::OrdMap<EntryRef, BTreeSet<ClaimRef>>;
pub(super) type Snapshots = BTreeMap<SelectionRef, Option<EntryBindings>>;

pub(super) fn resolve(
    selections: &BTreeMap<SelectionRef, HistorySelection>,
    contents: &imbl::OrdMap<EntryRef, BTreeMap<ClaimRef, HistoryContentObservation>>,
) -> Snapshots {
    if !selections.values().any(|selection| {
        matches!(
            selection.content_scope,
            HistoryContentScope::Snapshot { .. }
        )
    }) {
        return Snapshots::new();
    }
    let mut by_position = BTreeMap::<(AgentRef, SourcePosition, String), EntryBindings>::new();
    for (entry, observations) in contents {
        for (claim, observation) in observations {
            by_position
                .entry((
                    entry.scope().clone(),
                    observation.position.clone(),
                    observation.mapping_rule.clone(),
                ))
                .or_default()
                .entry(entry.clone())
                .or_default()
                .insert(claim.clone());
        }
    }
    let mut resolved = Snapshots::new();
    for root in selections.values().filter(|selection| {
        matches!(
            selection.content_scope,
            HistoryContentScope::Snapshot { .. }
        )
    }) {
        if resolved.contains_key(&root.selection_ref) {
            continue;
        }
        let mut path = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = &root.selection_ref;
        let mut base = loop {
            if let Some(known) = resolved.get(current) {
                break known.clone();
            }
            if !seen.insert(current.clone()) {
                break None;
            }
            let Some(selection) = selections.get(current) else {
                break None;
            };
            let HistoryContentScope::Snapshot {
                position,
                mapping_rule,
                previous,
            } = &selection.content_scope
            else {
                break None;
            };
            path.push(selection);
            let Some(previous) = previous else {
                break Some(EntryBindings::new());
            };
            let Some(prior) = selections.get(previous) else {
                break None;
            };
            let HistoryContentScope::Snapshot {
                position: prior_position,
                mapping_rule: prior_rule,
                ..
            } = &prior.content_scope
            else {
                break None;
            };
            if prior.agent != selection.agent
                || prior_rule != mapping_rule
                || !precedes(prior_position, position)
            {
                break None;
            }
            current = previous;
        };
        for selection in path.into_iter().rev() {
            if let Some(bindings) = &mut base {
                let HistoryContentScope::Snapshot {
                    position,
                    mapping_rule,
                    ..
                } = &selection.content_scope
                else {
                    unreachable!()
                };
                if let Some(additions) = by_position.get(&(
                    selection.agent.clone(),
                    position.clone(),
                    mapping_rule.clone(),
                )) {
                    for (entry, claims) in additions {
                        bindings.insert(entry.clone(), claims.clone());
                    }
                }
            }
            resolved.insert(selection.selection_ref.clone(), base.clone());
        }
    }
    resolved
}

fn precedes(earlier: &SourcePosition, later: &SourcePosition) -> bool {
    earlier.source == later.source
        && earlier.generation == later.generation
        && match (&earlier.locator, &later.locator) {
            (SourceLocator::Frame { ordinal: a }, SourceLocator::Frame { ordinal: b }) => a < b,
            (
                SourceLocator::Jsonl {
                    record_ordinal: a,
                    byte_end,
                    ..
                },
                SourceLocator::Jsonl {
                    record_ordinal: b,
                    byte_start,
                    ..
                },
            ) => a < b && byte_end <= byte_start,
            _ => false,
        }
}
