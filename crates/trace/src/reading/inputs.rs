//! Submitted bytes are the primary input presentation after an evidenced admission.
use super::*;

impl Index<'_> {
    pub(super) fn index_submitted_presentations(&mut self) {
        let mut deliveries = BTreeMap::<MessageRevisionRef, BTreeSet<DeliveryRef>>::new();
        for delivery in self.submitted.values().flatten() {
            if !self
                .reduction
                .delivery_stages
                .get(&delivery.delivery_ref)
                .into_iter()
                .flatten()
                .any(|(stage, _)| matches!(stage, DeliveryStage::Admitted { .. }))
            {
                continue;
            }
            if let Some(revision) = self
                .reduction
                .select_delivery_revision(&delivery.delivery_ref, None)
                .known()
            {
                if matches!(self.object(&AnyObjectRef::MessageRevision(revision.clone())), Some(TraceObject::MessageRevision(value)) if value.representation == Representation::Submitted)
                {
                    deliveries
                        .entry(revision.clone())
                        .or_default()
                        .insert(delivery.delivery_ref.clone());
                }
            }
        }
        for relation in &self.reduction.relations {
            let Relation::Representation {
                original,
                transformed,
                ..
            } = &relation.relation
            else {
                continue;
            };
            let Some(deliveries) = deliveries.get(original) else {
                continue;
            };
            self.input_presentations
                .entry(transformed.clone())
                .or_default()
                .extend(deliveries.iter().cloned());
            if let Some(TraceObject::MessageRevision(revision)) =
                self.object(&AnyObjectRef::MessageRevision(transformed.clone()))
            {
                let message = revision.message.clone();
                self.input_messages
                    .entry(message)
                    .or_default()
                    .extend(deliveries.iter().cloned());
            }
        }
    }
    pub(super) fn associated_deliveries<'b>(
        &'b self,
        entry: &HistoryEntry,
        eligible: &BTreeSet<MessageRevisionRef>,
    ) -> Vec<(DeliveryRef, &'b Delivery, Resolved<MessageRevisionRef>)> {
        let mut candidates: BTreeSet<_> = self
            .deliveries
            .get(&entry.entry_ref)
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let mut represented = BTreeSet::new();
        for revision in eligible {
            represented.extend(
                self.input_presentations
                    .get(revision)
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
            if let Some(TraceObject::MessageRevision(value)) =
                self.object(&AnyObjectRef::MessageRevision(revision.clone()))
            {
                // A shared native message identity can supply a later complete
                // presentation. Conflicting command associations cannot promote
                // an unrelated revision into another submitted request.
                if let Some(deliveries) = self
                    .input_messages
                    .get(&value.message)
                    .filter(|values| values.len() == 1)
                {
                    represented.extend(deliveries.iter().cloned());
                }
            }
        }
        candidates.extend(represented.iter().cloned());
        candidates.into_iter().filter_map(|reference| {
            let Some(TraceObject::Delivery(delivery)) = self.object(&AnyObjectRef::Delivery(reference.clone())) else { return None; };
            let selected = if matches!(delivery.route, DeliveryRoute::ServerCommand { .. }) && represented.contains(&reference) {
                if !matches!(&delivery.recipient, Recipient::Agent { agent: OwnerRef::Agent { agent } } if agent == &entry.containing_agent) { return None; }
                self.reduction.select_delivery_revision(&reference, None)
            } else {
                self.reduction.select_delivery_revision(&reference, Some(eligible))
            };
            (!matches!(selected, Resolved::Unknown)).then_some((reference, delivery, selected))
        }).collect()
    }
}
