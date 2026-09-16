//! Selected content observations are independent of native entry topology.
use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedEntryContents {
    pub slots: BTreeMap<u32, Resolved<Vec<HistoryContent>>>,
    pub complete: bool,
}

impl Reduction {
    pub fn select_delivery_revision(
        &self,
        delivery: &DeliveryRef,
        included: Option<&BTreeSet<MessageRevisionRef>>,
    ) -> Resolved<MessageRevisionRef> {
        let Some(observations) = self.delivery_revisions.get(delivery) else {
            return Resolved::Unknown;
        };
        let resolved = resolve_values(
            observations
                .iter()
                .filter(|(_, revision)| {
                    included.is_none_or(|included| included.contains(*revision))
                })
                .map(|(claim, revision)| (revision.clone(), claim.clone())),
        );
        let Resolved::Conflicted { alternatives } = &resolved else {
            return resolved;
        };
        let revision = |reference: &MessageRevisionRef| {
            let TraceObject::MessageRevision(revision) = self
                .object(&AnyObjectRef::MessageRevision(reference.clone()))?
                .declaration
                .known()?
            else {
                return None;
            };
            Some(revision)
        };
        let Some(first) = alternatives
            .first()
            .and_then(|(reference, _)| revision(reference))
        else {
            return resolved;
        };
        // A correction of a message's displayed value does not rewrite a past
        // delivery. Only withdrawn binding claims or agreeing complete observed
        // presentations can resolve this particular delivery's revision evidence.
        if alternatives.iter().all(|(reference, _)| {
            revision(reference).is_some_and(|value| same_complete_presentation(first, value))
        }) {
            Resolved::Known {
                value: alternatives
                    .iter()
                    .map(|(reference, _)| reference)
                    .min()
                    .unwrap()
                    .clone(),
                claims: alternatives
                    .iter()
                    .flat_map(|(_, claims)| claims.iter().cloned())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            }
        } else {
            resolved
        }
    }

    pub fn entry_contents(
        &self,
        entry: &EntryRef,
        selection: Option<&SelectionRef>,
    ) -> ResolvedEntryContents {
        let Some(observations) = self.history_contents.get(entry) else {
            return ResolvedEntryContents::default();
        };
        let allowed = match selection {
            None => None,
            Some(selection) => match self.selection_content_observations.get(selection) {
                Some(Some(allowed)) => Some(allowed),
                _ => return ResolvedEntryContents::default(),
            },
        };
        let observations: Vec<_> = observations
            .iter()
            .filter(|(claim, _)| allowed.is_none_or(|allowed| allowed.contains(*claim)))
            .collect();
        let slots: BTreeSet<_> = observations
            .iter()
            .flat_map(|(_, observation)| observation.slots.iter().map(|slot| slot.slot))
            .collect();
        let mut result = ResolvedEntryContents {
            slots: BTreeMap::new(),
            complete: observations
                .iter()
                .any(|(_, observation)| observation.complete),
        };
        for slot in slots {
            let values = observations.iter().filter_map(|(claim, observation)| {
                match observation.slots.iter().find(|value| value.slot == slot) {
                    Some(value) => Some((vec![value.content.clone()], (*claim).clone())),
                    None if observation.complete => Some((vec![], (*claim).clone())),
                    None => None,
                }
            });
            let mut resolved = resolve_values(values);
            if let Resolved::Conflicted { alternatives } = &resolved {
                // A slot can refer to several immutable observations of the same
                // proved message. This retains candidates; it does not merge
                // their revisions or select bytes from an unobserved source.
                let mut identity = None;
                let mut candidates = BTreeSet::new();
                let mut claims = BTreeSet::new();
                let compatible = alternatives.iter().all(|(contents, support)| {
                    let [HistoryContent::Message(reference)] = contents.as_slice() else {
                        return false;
                    };
                    let Some(TraceObject::MessageRevision(revision)) = self
                        .object(&AnyObjectRef::MessageRevision(reference.clone()))
                        .and_then(|object| object.declaration.known())
                    else {
                        return false;
                    };
                    if !matches!(
                        self.object(&AnyObjectRef::Message(revision.message.clone()))
                            .and_then(|object| object.declaration.known()),
                        Some(TraceObject::Message(_))
                    ) {
                        return false;
                    }
                    let next = (revision.message.clone(), revision.representation);
                    if identity.as_ref().is_some_and(|identity| identity != &next) {
                        return false;
                    }
                    identity = Some(next);
                    candidates.insert(reference.clone());
                    claims.extend(support.iter().cloned());
                    true
                });
                if compatible {
                    resolved = Resolved::Known {
                        value: candidates
                            .into_iter()
                            .map(HistoryContent::Message)
                            .collect(),
                        claims: claims.into_iter().collect(),
                    };
                }
            }
            result.slots.insert(slot, resolved);
        }
        result
    }

    /// Select one eligible presentation while retaining distinct revision IDs.
    /// Equality is considered only after a Message identity is already proved.
    pub fn select_message_in(
        &self,
        message: &MessageRef,
        included: &BTreeSet<MessageRevisionRef>,
    ) -> Resolved<MessageRevisionRef> {
        let Some(history) = self.message(message) else {
            return Resolved::Unknown;
        };
        let selected = history.select_in(included);
        let Resolved::Conflicted { alternatives } = &selected else {
            return selected;
        };
        let Some((first, _)) = alternatives.first() else {
            return selected;
        };
        let revision = |reference: &MessageRevisionRef| {
            let TraceObject::MessageRevision(revision) = self
                .object(&AnyObjectRef::MessageRevision(reference.clone()))?
                .declaration
                .known()?
            else {
                return None;
            };
            (revision.message == *message
                && matches!(
                    revision.form,
                    RevisionForm::Complete
                        | RevisionForm::Final { .. }
                        | RevisionForm::Correction { .. }
                ))
            .then_some(revision)
        };
        let Some(first) = revision(first) else {
            return selected;
        };
        let agrees = alternatives.iter().all(|(reference, _)| {
            revision(reference).is_some_and(|value| same_complete_presentation(first, value))
        });
        if !agrees {
            return selected;
        }
        Resolved::Known {
            value: alternatives
                .iter()
                .map(|(reference, _)| reference)
                .min()
                .unwrap()
                .clone(),
            claims: alternatives
                .iter()
                .flat_map(|(_, claims)| claims.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }
}

fn same_complete_presentation(left: &MessageRevision, right: &MessageRevision) -> bool {
    let complete = |revision: &MessageRevision| {
        matches!(
            revision.form,
            RevisionForm::Complete | RevisionForm::Final { .. } | RevisionForm::Correction { .. }
        )
    };
    complete(left)
        && complete(right)
        && left.message == right.message
        && left.representation == right.representation
        && left.parts.len() == right.parts.len()
        && left.parts.iter().zip(&right.parts).all(|(left, right)| {
            left.kind == right.kind && left.payload == right.payload
                // A bytes handle establishes the whole immutable byte value.
                // Manifest completeness is not yet a reducer input; equal
                // handles alone cannot certify complete observed content.
                && left.payload.kind() == PayloadKind::Bytes
                && left.media_type == right.media_type && left.encoding == right.encoding
        })
}
