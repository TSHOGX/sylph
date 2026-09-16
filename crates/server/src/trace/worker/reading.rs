use super::*;
use crate::trace::{
    generation::{PayloadAnchor, PayloadAnchorChange},
    reading_store::{read_scope, ReadRowChange},
};
use sylph_trace::reading::{PayloadPreview, ReadingProjection};

/// Immutable cache of exactly the previous committed reader rows. Cloning a
/// candidate unit shares these bytes; failed or cancelled units cannot mutate it.
#[derive(Default)]
pub(super) struct MaterializedReading {
    rows: BTreeMap<(String, String), ReadRowChange>,
    anchors: BTreeMap<String, PayloadAnchor>,
    composed: BTreeMap<PayloadRef, sylph_trace::reading::ComposedPayload>,
}
impl MaterializedReading {
    fn from_projection(
        projection: &ReadingProjection,
        epochs: &BTreeMap<AnyObjectRef, IngestionEpoch>,
    ) -> Result<Self> {
        Ok(Self {
            rows: rows(projection, epochs)?,
            anchors: anchors(projection)?,
            composed: projection.composed_payloads.clone(),
        })
    }
}

impl ProjectionWorker {
    pub(super) async fn stage_composed_reading(
        &self,
        generation: &GenerationStore,
        unit: &ProjectionUnit,
    ) -> Result<()> {
        for (epoch, reference, manifest) in &unit.composed_payloads {
            let ticket = self
                .catalogue
                .begin_upload(
                    PinOwnerKind::Generation,
                    &generation.manifest().generation,
                    reference.clone(),
                    epoch.clone(),
                    crate::models::now_ms(),
                )
                .await?;
            if self.evidence.stage_manifest(manifest).await? != *reference {
                return Err(WorkerError::Task("composed payload digest mismatch".into()));
            }
            self.catalogue.finish_upload(&ticket).await?;
        }
        Ok(())
    }
    pub(super) async fn materialize_reading(
        &self,
        generation: &GenerationStore,
        old: &Reduction,
        unit: &mut ProjectionUnit,
    ) -> Result<()> {
        // Reconstruct after restart before changing preview metadata. Recomputing
        // "before" with new metadata masks a late payload correction as unchanged.
        let before = match &unit.next.reading {
            Some(previous) => previous.clone(),
            None => Arc::new(MaterializedReading::from_projection(
                &sylph_trace::reading::project(old, &unit.next.previews, 32),
                &unit.next.object_epochs,
            )?),
        };
        for (_, payload) in &unit.payloads {
            let length = payload.bytes.len();
            let limit = generation.limits().max_preview_bytes;
            let text = if payload.encoding == PayloadEncoding::Utf8 {
                std::str::from_utf8(&payload.bytes).ok()
            } else {
                None
            };
            let mut end = text.map_or(0, |text| text.len().min(limit));
            if let Some(text) = text {
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
            }
            let preview = PayloadPreview {
                chunks: payload
                    .manifest
                    .as_ref()
                    .map(|manifest| manifest.chunks.clone()),
                preview_limit: limit,
                preview: text.map_or_else(String::new, |text| text[..end].into()),
                preview_truncated: text.is_some_and(|text| text.len() > end),
                total_bytes: Some(length as u64),
                ranges: if length == 0 {
                    vec![]
                } else {
                    vec![PayloadRange {
                        start: 0,
                        end_exclusive: length as u64,
                    }]
                },
                availability: Some(ReadAvailability::Available),
            };
            unit.batch.mapper_states.insert(
                format!("read_preview:{}", payload.reference),
                serde_json::to_value(&preview)?,
            );
            unit.next
                .previews
                .insert(payload.reference.clone(), preview);
        }
        let refs: BTreeSet<_> = unit
            .next
            .reduction
            .objects
            .values()
            .filter_map(|object| object.declaration.known())
            .flat_map(TraceObject::payload_refs)
            .cloned()
            .collect();
        for payload in refs {
            if unit.next.previews.contains_key(&payload) {
                continue;
            }
            let length = self.evidence.payload_length(&payload).await?;
            let manifest = self.evidence.payload_manifest(&payload).await?;
            let (ranges, availability) =
                match manifest.as_ref().map(|manifest| &manifest.completeness) {
                    Some(PayloadCompleteness::KnownRanges { ranges }) => (
                        ranges.clone(),
                        ReadAvailability::Partial {
                            reason: "Only recorded payload ranges are available".into(),
                        },
                    ),
                    Some(PayloadCompleteness::Unavailable { reason }) => (
                        vec![],
                        ReadAvailability::Unavailable {
                            reason: reason.clone(),
                        },
                    ),
                    Some(PayloadCompleteness::SourceRedacted { reason }) => (
                        vec![],
                        ReadAvailability::Redacted {
                            reason: reason.clone(),
                        },
                    ),
                    Some(PayloadCompleteness::Opaque { reason }) => (
                        vec![],
                        ReadAvailability::Opaque {
                            reason: reason.clone(),
                        },
                    ),
                    _ => (
                        length
                            .filter(|length| *length > 0)
                            .map_or_else(Vec::new, |length| {
                                vec![PayloadRange {
                                    start: 0,
                                    end_exclusive: length,
                                }]
                            }),
                        ReadAvailability::Available,
                    ),
                };
            let preview = PayloadPreview {
                chunks: manifest.as_ref().map(|manifest| manifest.chunks.clone()),
                preview_limit: generation.limits().max_preview_bytes,
                total_bytes: length,
                ranges,
                availability: Some(availability),
                ..PayloadPreview::default()
            };
            unit.batch.mapper_states.insert(
                format!("read_preview:{payload}"),
                serde_json::to_value(&preview)?,
            );
            unit.next.previews.insert(payload, preview);
        }
        let after = sylph_trace::reading::project(&unit.next.reduction, &unit.next.previews, 32);
        for (reference, composed) in &after.composed_payloads {
            if before
                .composed
                .get(reference)
                .is_some_and(|previous| previous.revisions == composed.revisions)
            {
                continue;
            }
            let epochs: BTreeSet<_> = composed
                .revisions
                .iter()
                .map(|revision| {
                    unit.next
                        .object_epochs
                        .get(&AnyObjectRef::MessageRevision(revision.clone()))
                        .cloned()
                        .ok_or_else(|| {
                            WorkerError::Task("composed content lacks its revision epoch".into())
                        })
                })
                .collect::<Result<_>>()?;
            let claims: BTreeSet<_> = composed
                .revisions
                .iter()
                .filter_map(|revision| {
                    unit.next
                        .reduction
                        .object(&AnyObjectRef::MessageRevision(revision.clone()))
                })
                .filter_map(|object| match &object.declaration {
                    sylph_trace::reducer::Resolved::Known { claims, .. } => Some(claims),
                    _ => None,
                })
                .flatten()
                .cloned()
                .collect();
            let evidence = unit
                .next
                .reducer
                .claims()
                .filter(|claim| claims.contains(&claim.claim_ref))
                .flat_map(|claim| claim.evidence.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            unit.batch.payload_derivations.push(PayloadDerivation {
                payload: reference.clone(),
                evidence,
                selector: serde_json::to_string(&composed.revisions)?,
                rule: "trace/append-chain".into(),
                media_type: composed.manifest.media_type.clone(),
                encoding: composed.manifest.encoding,
            });
            for epoch in epochs {
                unit.composed_payloads
                    .push((epoch, reference.clone(), composed.manifest.clone()));
            }
        }
        let after = MaterializedReading::from_projection(&after, &unit.next.object_epochs)?;
        for (key, row) in &after.rows {
            if before.rows.get(key).is_none_or(|old| {
                old.body != row.body
                    || old.order_key != row.order_key
                    || old.scope_epoch != row.scope_epoch
            }) {
                unit.batch.read_rows.push(row.clone());
            }
        }
        for (key, row) in &before.rows {
            if !after.rows.contains_key(key) {
                unit.batch.read_rows.push(ReadRowChange {
                    body: None,
                    ..row.clone()
                });
            }
        }
        for (key, anchor) in &after.anchors {
            if !before.anchors.contains_key(key) {
                unit.batch.payload_anchors.push(PayloadAnchorChange {
                    anchor: anchor.clone(),
                    present: true,
                });
            }
        }
        for (key, anchor) in &before.anchors {
            if !after.anchors.contains_key(key) {
                unit.batch.payload_anchors.push(PayloadAnchorChange {
                    anchor: anchor.clone(),
                    present: false,
                });
            }
        }
        unit.next.reading = Some(Arc::new(after));
        Ok(())
    }
}

fn rows(
    projection: &ReadingProjection,
    epochs: &BTreeMap<AnyObjectRef, IngestionEpoch>,
) -> Result<BTreeMap<(String, String), ReadRowChange>> {
    let mut rows = BTreeMap::new();
    let mut insert = |scope: String,
                      address: String,
                      anchor: Option<TraceAddress>,
                      index: usize,
                      body: serde_json::Value,
                      subject: &AnyObjectRef|
     -> Result<()> {
        let Some(epoch) = epochs
            .get(subject)
            .or_else(|| epochs.get(&AnyObjectRef::Agent(subject.scope().clone())))
        else {
            return Err(WorkerError::Task(
                "reader projection lacks an evidence scope epoch".into(),
            ));
        };
        let row = ReadRowChange {
            scope: scope.clone(),
            address: address.clone(),
            anchor,
            order_key: (index as u64 + 1).to_be_bytes().to_vec(),
            body: Some(body),
            scope_epoch: epoch.clone(),
        };
        rows.insert((scope, address), row);
        Ok(())
    };
    for (agent, header) in &projection.headers {
        insert(
            read_scope(agent, None, "header"),
            agent.encode(),
            Some(TraceAddress::of(&AnyObjectRef::Agent(agent.clone()))),
            0,
            serde_json::to_value(header)?,
            &AnyObjectRef::Agent(agent.clone()),
        )?;
    }
    for (scope, units) in &projection.windows {
        for (index, unit) in units.iter().enumerate() {
            insert(
                read_scope(&scope.agent, scope.selection.as_ref(), "history"),
                unit.address.clone(),
                Some(unit.anchor.clone()),
                index,
                serde_json::to_value(unit)?,
                &unit.anchor.object(),
            )?;
        }
    }
    for (agent, directory) in &projection.directories {
        for (index, row) in directory.iter().enumerate() {
            insert(
                read_scope(agent, None, "relations"),
                row.agent.address.as_str().into(),
                Some(TraceAddress::of(&AnyObjectRef::Agent(
                    row.agent.address.agent(),
                ))),
                index,
                serde_json::to_value(row)?,
                &AnyObjectRef::Agent(row.agent.address.agent()),
            )?;
        }
    }
    for (agent, histories) in &projection.histories {
        for (index, row) in histories.iter().enumerate() {
            insert(
                read_scope(agent, None, "histories"),
                row.selection.encode(),
                Some(TraceAddress::of(&AnyObjectRef::HistorySelection(
                    row.selection.clone(),
                ))),
                index,
                serde_json::to_value(row)?,
                &AnyObjectRef::HistorySelection(row.selection.clone()),
            )?;
        }
    }
    for (reference, detail) in &projection.objects {
        let mut summary = detail.clone();
        summary.relations.clear();
        match &mut summary.content {
            ReadObjectContent::Operation {
                results, resources, ..
            } => {
                results.clear();
                resources.clear();
            }
            ReadObjectContent::Context { memberships, .. } => memberships.clear(),
            _ => {}
        }
        insert(
            read_scope(reference.scope(), None, "objects"),
            reference.encode(),
            Some(TraceAddress::of(reference)),
            0,
            serde_json::to_value(summary)?,
            reference,
        )?;
        match &detail.content {
            ReadObjectContent::Operation {
                results, resources, ..
            } => {
                for (index, result) in results.iter().enumerate() {
                    insert(
                        read_scope(
                            reference.scope(),
                            None,
                            &format!("object-results:{}", reference.encode()),
                        ),
                        result.reference.as_str().into(),
                        Some(result.reference.clone()),
                        index,
                        serde_json::to_value(result)?,
                        reference,
                    )?;
                }
                for (index, resource) in resources.iter().enumerate() {
                    insert(
                        read_scope(
                            reference.scope(),
                            None,
                            &format!("object-resources:{}", reference.encode()),
                        ),
                        resource.target.as_str().into(),
                        Some(resource.target.clone()),
                        index,
                        serde_json::to_value(resource)?,
                        reference,
                    )?;
                }
            }
            ReadObjectContent::Context { memberships, .. } => {
                for (index, member) in memberships.iter().enumerate() {
                    insert(
                        read_scope(
                            reference.scope(),
                            None,
                            &format!("object-memberships:{}", reference.encode()),
                        ),
                        member.target.as_str().into(),
                        Some(member.target.clone()),
                        index,
                        serde_json::to_value(member)?,
                        reference,
                    )?;
                }
            }
            _ => {}
        }
    }
    for (reference, relations) in &projection.object_relations {
        for (index, relation) in relations.iter().enumerate() {
            insert(
                read_scope(
                    reference.scope(),
                    None,
                    &format!("object-relations:{}", reference.encode()),
                ),
                serde_json::to_string(&(relation.target.as_str(), &relation.label))?,
                Some(relation.target.clone()),
                index,
                serde_json::to_value(relation)?,
                reference,
            )?;
        }
    }
    for (scope, references) in &projection.selected_objects {
        for reference in references {
            insert(
                read_scope(&scope.agent, scope.selection.as_ref(), "selected-objects"),
                reference.encode(),
                Some(TraceAddress::of(reference)),
                0,
                serde_json::json!({}),
                &AnyObjectRef::Agent(scope.agent.clone()),
            )?;
        }
    }
    for (scope, parts) in &projection.parts {
        for (index, part) in parts.iter().enumerate() {
            insert(
                scope.clone(),
                part.part_ref.encode(),
                Some(part.payload.via_ref.clone()),
                index,
                serde_json::to_value(part)?,
                &part.payload.via_ref.object(),
            )?;
        }
    }
    // Selected detail collections carry the same bounded rows under a distinct
    // cursor scope. An archive cursor cannot accidentally widen an inspection.
    let mut selected_rows = Vec::new();
    for (scope, references) in &projection.selected_objects {
        for reference in references {
            for kind in ["relations", "results", "resources", "memberships"] {
                let collection = format!("object-{kind}:{}", reference.encode());
                let archive_scope = read_scope(reference.scope(), None, &collection);
                for ((key, _), row) in rows
                    .range((archive_scope.clone(), String::new())..)
                    .take_while(|((key, _), _)| key == &archive_scope)
                {
                    let _ = key;
                    let mut row = row.clone();
                    row.scope =
                        read_scope(reference.scope(), scope.selection.as_ref(), &collection);
                    selected_rows.push(row);
                }
            }
        }
    }
    for row in selected_rows {
        rows.insert((row.scope.clone(), row.address.clone()), row);
    }
    Ok(rows)
}

fn anchors(projection: &ReadingProjection) -> Result<BTreeMap<String, PayloadAnchor>> {
    let mut anchors = BTreeMap::new();
    let mut add = |payload: &PayloadAccess| -> Result<()> {
        let anchor = PayloadAnchor {
            via_ref: payload.via_ref.object(),
            containing_agent: payload.containing_agent.agent(),
            selection: payload.selection.clone(),
            payload_ref: payload.payload_ref.clone(),
            body_revision: payload.body_revision.clone(),
            representation: payload.representation,
            ranges: payload.available_ranges.clone(),
        };
        anchors.insert(serde_json::to_string(&anchor)?, anchor);
        Ok(())
    };
    for units in projection.windows.values() {
        for unit in units {
            let payloads: Vec<_> = match &unit.content {
                ReadingContent::Message { parts, .. } => {
                    parts.iter().map(|part| &part.payload).collect()
                }
                ReadingContent::Unknown { payloads, .. } => payloads.iter().collect(),
                ReadingContent::ContextBoundary {
                    summary: Some(summary),
                    ..
                } => vec![&summary.payload],
                _ => vec![],
            };
            for payload in payloads {
                add(payload)?;
            }
        }
    }
    for object in projection.objects.values() {
        match &object.content {
            ReadObjectContent::Operation { input, results, .. } => {
                for payload in input {
                    add(payload)?;
                }
                for result in results {
                    for payload in &result.payloads {
                        add(payload)?;
                    }
                }
            }
            ReadObjectContent::Resource { payloads, .. }
            | ReadObjectContent::Context { payloads, .. }
            | ReadObjectContent::Generic { payloads, .. } => {
                for payload in payloads {
                    add(payload)?;
                }
            }
            _ => {}
        }
    }
    for parts in projection.parts.values() {
        for part in parts {
            add(&part.payload)?;
        }
    }
    let mut archive_anchors = BTreeMap::<AnyObjectRef, Vec<PayloadAnchor>>::new();
    for anchor in anchors.values().filter(|anchor| anchor.selection.is_none()) {
        archive_anchors
            .entry(anchor.via_ref.clone())
            .or_default()
            .push(anchor.clone());
    }
    for (scope, references) in &projection.selected_objects {
        for reference in references {
            for anchor in archive_anchors.get(reference).into_iter().flatten() {
                if anchor.containing_agent == scope.agent {
                    let mut anchor = anchor.clone();
                    anchor.selection = scope.selection.clone();
                    anchors.insert(serde_json::to_string(&anchor)?, anchor);
                }
            }
        }
    }
    Ok(anchors)
}
