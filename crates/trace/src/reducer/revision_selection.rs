//! Source-domain ordering and immutable message revision selection.

use super::*;

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ComparisonWork {
    pub source: usize,
    pub identity: usize,
}

#[cfg(test)]
thread_local! {
    static COMPARISON_WORK: std::cell::Cell<ComparisonWork> = const {
        std::cell::Cell::new(ComparisonWork { source: 0, identity: 0 })
    };
}

#[cfg(test)]
pub(super) fn reset_comparison_work() {
    COMPARISON_WORK.set(ComparisonWork::default());
}

#[cfg(test)]
pub(super) fn comparison_work() -> ComparisonWork {
    COMPARISON_WORK.get()
}

#[inline]
pub(super) fn source_comparison() {
    #[cfg(test)]
    {
        let mut work = COMPARISON_WORK.get();
        work.source += 1;
        COMPARISON_WORK.set(work);
    }
}

#[inline]
fn identity_comparison() {
    #[cfg(test)]
    {
        let mut work = COMPARISON_WORK.get();
        work.identity += 1;
        COMPARISON_WORK.set(work);
    }
}

/// A source can expose several incomparable coordinate families. Snapshot and
/// occurrence identifiers establish equality only; their spelling never orders
/// revisions, and JSONL uses byte_start rather than the mapped record ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum OrderedLocator {
    Jsonl,
    Frame,
}

#[derive(Debug, Clone, Copy)]
struct OrderedDomain<'a> {
    source: &'a SourceRef,
    generation: u64,
    locator: OrderedLocator,
}

impl PartialEq for OrderedDomain<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for OrderedDomain<'_> {}
impl PartialOrd for OrderedDomain<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OrderedDomain<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        source_comparison();
        (self.source, self.generation, self.locator).cmp(&(
            other.source,
            other.generation,
            other.locator,
        ))
    }
}

fn ordered_coordinate(position: &SourcePosition) -> Option<(OrderedDomain<'_>, u64)> {
    let (locator, coordinate) = match position.locator {
        SourceLocator::Jsonl { byte_start, .. } => (OrderedLocator::Jsonl, byte_start),
        SourceLocator::Frame { ordinal } => (OrderedLocator::Frame, ordinal),
        SourceLocator::Snapshot { .. } | SourceLocator::Occurrence { .. } => return None,
    };
    Some((
        OrderedDomain {
            source: &position.source,
            generation: position.generation,
            locator,
        },
        coordinate,
    ))
}

fn coordinate_order(left: u64, right: u64) -> std::cmp::Ordering {
    source_comparison();
    left.cmp(&right)
}

pub(super) fn source_order(a: &SourcePosition, b: &SourcePosition) -> Option<std::cmp::Ordering> {
    match (ordered_coordinate(a), ordered_coordinate(b)) {
        (Some((a_domain, a_coordinate)), Some((b_domain, b_coordinate))) => {
            (a_domain == b_domain).then(|| coordinate_order(a_coordinate, b_coordinate))
        }
        (None, None) => {
            source_comparison();
            (a == b).then_some(std::cmp::Ordering::Equal)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct RevisionIdentity<'a>(&'a MessageRevisionRef);

impl PartialEq for RevisionIdentity<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for RevisionIdentity<'_> {}
impl PartialOrd for RevisionIdentity<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RevisionIdentity<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        identity_comparison();
        self.0.cmp(other.0)
    }
}

/// The same surviving seals govern selection and late-segment diagnostics. Build
/// this after filtering a selected history: an excluded correction cannot suppress
/// an included final. A correction is a seal even when its target is unavailable.
pub(super) struct RevisionSelection<'a, 'values> {
    values: &'values [&'a RevisionCandidate],
    seals: Vec<&'a RevisionCandidate>,
    earliest_seals: BTreeMap<OrderedDomain<'a>, u64>,
}

impl<'a, 'values> RevisionSelection<'a, 'values> {
    pub(super) fn new(values: &'values [&'a RevisionCandidate]) -> Self {
        let replaced: BTreeSet<_> = values
            .iter()
            .filter_map(|value| match &value.form {
                RevisionForm::Correction { replaces } => Some(RevisionIdentity(replaces)),
                _ => None,
            })
            .collect();
        let seals: Vec<_> = values
            .iter()
            .copied()
            .filter(|value| {
                !replaced.contains(&RevisionIdentity(&value.reference))
                    && matches!(
                        value.form,
                        RevisionForm::Complete
                            | RevisionForm::Final { .. }
                            | RevisionForm::Correction { .. }
                    )
            })
            .collect();
        let mut earliest_seals: BTreeMap<OrderedDomain<'a>, u64> = BTreeMap::new();
        for value in &seals {
            if let Some((domain, coordinate)) = value.position.as_ref().and_then(ordered_coordinate)
            {
                earliest_seals
                    .entry(domain)
                    .and_modify(|minimum| {
                        if coordinate_order(coordinate, *minimum).is_lt() {
                            *minimum = coordinate;
                        }
                    })
                    .or_insert(coordinate);
            }
        }
        Self {
            values,
            seals,
            earliest_seals,
        }
    }

    pub(super) fn sealed(&self) -> bool {
        !self.seals.is_empty()
    }

    pub(super) fn is_late(&self, position: Option<&SourcePosition>) -> bool {
        let Some((domain, coordinate)) = position.and_then(ordered_coordinate) else {
            return false;
        };
        self.earliest_seals
            .get(&domain)
            .is_some_and(|minimum| coordinate_order(coordinate, *minimum).is_gt())
    }

    pub(super) fn select(&self) -> Resolved<MessageRevisionRef> {
        if self.sealed() {
            return resolve_revision_values(self.seals.iter().copied());
        }
        let mut maxima: BTreeMap<OrderedDomain<'a>, u64> = BTreeMap::new();
        for value in self.values {
            if let Some((domain, coordinate)) = value.position.as_ref().and_then(ordered_coordinate)
            {
                maxima
                    .entry(domain)
                    .and_modify(|maximum| {
                        if coordinate_order(coordinate, *maximum).is_gt() {
                            *maximum = coordinate;
                        }
                    })
                    .or_insert(coordinate);
            }
        }
        // Preserve candidate order, including every support claim at tied maxima.
        // In the no-seal case the original rule compares all candidates, including
        // correction targets; removing replaced candidates here would change it.
        resolve_revision_values(self.values.iter().copied().filter(|value| {
            value
                .position
                .as_ref()
                .and_then(ordered_coordinate)
                .is_none_or(|(domain, coordinate)| {
                    coordinate_order(coordinate, maxima[&domain]).is_eq()
                })
        }))
    }
}

fn resolve_revision_values<'a>(
    values: impl Iterator<Item = &'a RevisionCandidate>,
) -> Resolved<MessageRevisionRef> {
    // Map ordering is only an index. The vector retains the first occurrence of
    // each alternative and each support claim's input order, matching resolve_values.
    let mut indexes = BTreeMap::new();
    let mut alternatives: Vec<(MessageRevisionRef, Vec<ClaimRef>)> = Vec::new();
    for value in values {
        let index = *indexes
            .entry(RevisionIdentity(&value.reference))
            .or_insert_with(|| {
                alternatives.push((value.reference.clone(), Vec::new()));
                alternatives.len() - 1
            });
        alternatives[index].1.push(value.claim.clone());
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

#[cfg(test)]
mod tests;
