//! Reconstruct only evidenced append chains. Snapshots and finals replace the body;
//! chunks retain each repeated occurrence rather than pointing into a guessed final.
use super::*;

#[derive(Debug, Clone)]
pub struct ComposedPayload {
    pub manifest: PayloadManifest,
    pub revisions: Vec<MessageRevisionRef>,
}

pub(super) fn resolve(
    reduction: &Reduction,
    payloads: &mut BTreeMap<PayloadRef, PayloadPreview>,
) -> (
    BTreeMap<MessageRevisionRef, Vec<ContentPart>>,
    BTreeMap<PayloadRef, ComposedPayload>,
    BTreeMap<MessageRevisionRef, String>,
) {
    let mut parts = BTreeMap::new();
    let mut composed = BTreeMap::new();
    let mut unavailable = BTreeMap::new();
    let mut requested: BTreeSet<_> = reduction
        .objects
        .values()
        .filter_map(|object| object.declaration.known())
        .filter_map(|object| match object {
            TraceObject::Delivery(delivery) => reduction
                .select_delivery_revision(&delivery.delivery_ref, None)
                .known()
                .cloned(),
            _ => None,
        })
        .collect();
    for (selection, entries) in reduction
        .histories
        .values()
        .map(|history| (None, history.archive_order.as_slice()))
        .chain(
            reduction
                .selection_memberships
                .iter()
                .filter_map(|(selection, members)| {
                    members.as_deref().map(|members| (Some(selection), members))
                }),
        )
    {
        let mut messages = BTreeMap::<MessageRef, BTreeSet<MessageRevisionRef>>::new();
        for entry in entries {
            let contents = reduction.entry_contents(entry, selection);
            for content in contents
                .slots
                .values()
                .filter_map(Resolved::known)
                .flatten()
            {
                let HistoryContent::Message(reference) = content else {
                    continue;
                };
                let Some(TraceObject::MessageRevision(revision)) = reduction
                    .object(&AnyObjectRef::MessageRevision(reference.clone()))
                    .and_then(|object| object.declaration.known())
                else {
                    continue;
                };
                messages
                    .entry(revision.message.clone())
                    .or_default()
                    .insert(reference.clone());
            }
        }
        for (message, included) in messages {
            if let Some(reference) = reduction
                .select_message_in(&message, &included)
                .known()
                .cloned()
            {
                requested.insert(reference);
            }
        }
    }
    for selected in &requested {
        let Some(chain) = chain(reduction, selected) else {
            unavailable.insert(
                selected.clone(),
                "The observed content has an unresolved append predecessor".into(),
            );
            continue;
        };
        if chain.len() < 2 {
            continue;
        }
        let mut ordered = Vec::<(ContentPart, Vec<PayloadRef>)>::new();
        let mut conflict = false;
        for revision in &chain {
            for part in &revision.parts {
                if let Some((prior, segments)) = ordered
                    .iter_mut()
                    .find(|(prior, _)| prior.part_ref == part.part_ref)
                {
                    if prior.kind != part.kind
                        || prior.media_type != part.media_type
                        || prior.encoding != part.encoding
                    {
                        conflict = true;
                        break;
                    }
                    segments.push(part.payload.clone());
                } else {
                    ordered.push((part.clone(), vec![part.payload.clone()]));
                }
            }
        }
        if conflict {
            unavailable.insert(
                selected.clone(),
                "Streamed part metadata conflicts with its predecessor".into(),
            );
            continue;
        }
        let mut resolved = Vec::new();
        for (mut part, segments) in ordered {
            if segments.len() > 1 {
                if let Some((reference, manifest, preview)) = compose(&part, &segments, payloads) {
                    let revisions: Vec<_> = chain
                        .iter()
                        .map(|revision| revision.revision_ref.clone())
                        .collect();
                    composed
                        .entry(reference.clone())
                        .and_modify(|existing: &mut ComposedPayload| {
                            existing.revisions.extend(revisions.iter().cloned());
                            existing.revisions.sort();
                            existing.revisions.dedup();
                        })
                        .or_insert(ComposedPayload {
                            manifest,
                            revisions,
                        });
                    payloads.insert(reference.clone(), preview);
                    part.payload = reference;
                } else {
                    unavailable.insert(
                        selected.clone(),
                        "Some observed append payload ranges are unavailable".into(),
                    );
                    conflict = true;
                    break;
                }
            }
            resolved.push(part);
        }
        if !conflict {
            parts.insert(selected.clone(), resolved);
        }
    }
    (parts, composed, unavailable)
}

fn chain<'a>(
    reduction: &'a Reduction,
    selected: &MessageRevisionRef,
) -> Option<Vec<&'a MessageRevision>> {
    let mut reference = selected;
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    loop {
        if !seen.insert(reference.clone()) {
            return None;
        }
        let TraceObject::MessageRevision(revision) = reduction
            .object(&AnyObjectRef::MessageRevision(reference.clone()))?
            .declaration
            .known()?
        else {
            return None;
        };
        if result.first().is_some_and(|first: &&MessageRevision| {
            first.message != revision.message || first.representation != revision.representation
        }) {
            return None;
        }
        result.push(revision);
        match &revision.form {
            RevisionForm::Append {
                predecessor: Some(previous),
            } => reference = previous,
            _ => break,
        }
    }
    result.reverse();
    Some(result)
}

fn compose(
    part: &ContentPart,
    segments: &[PayloadRef],
    payloads: &BTreeMap<PayloadRef, PayloadPreview>,
) -> Option<(PayloadRef, PayloadManifest, PayloadPreview)> {
    let mut chunks = Vec::new();
    let mut length = 0u64;
    let mut preview = String::new();
    let mut truncated = false;
    for segment in segments {
        let metadata = payloads.get(segment)?;
        if metadata.availability != Some(ReadAvailability::Available) {
            return None;
        }
        let size = metadata.total_bytes?;
        let source_chunks = if segment.kind() == PayloadKind::Bytes {
            if size == 0 {
                vec![]
            } else {
                vec![PayloadChunk {
                    digest: segment.clone(),
                    chunk_offset: 0,
                    length: size,
                    payload_offset: 0,
                }]
            }
        } else {
            metadata.chunks.clone()?
        };
        for mut chunk in source_chunks {
            chunk.payload_offset = chunk.payload_offset.checked_add(length)?;
            chunks.push(chunk);
        }
        length = length.checked_add(size)?;
        // The preview limit is supplied by the producer of payload metadata. Once
        // a segment is truncated, later text cannot fill that omitted interval.
        if !truncated {
            let remaining = metadata.preview_limit.saturating_sub(preview.len());
            let mut end = metadata.preview.len().min(remaining);
            while !metadata.preview.is_char_boundary(end) {
                end -= 1;
            }
            preview.push_str(&metadata.preview[..end]);
            truncated = metadata.preview_truncated || end < metadata.preview.len();
        }
    }
    let manifest = PayloadManifest {
        chunks,
        media_type: part
            .media_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".into()),
        encoding: part.encoding,
        decoded_length: Some(length),
        completeness: PayloadCompleteness::Complete,
    };
    let reference =
        PayloadRef::manifest(blake3::hash(&manifest.canonical_bytes().ok()?).to_hex()).ok()?;
    let metadata = PayloadPreview {
        preview,
        preview_truncated: truncated,
        total_bytes: Some(length),
        ranges: if length > 0 {
            vec![PayloadRange {
                start: 0,
                end_exclusive: length,
            }]
        } else {
            vec![]
        },
        availability: Some(ReadAvailability::Available),
        chunks: Some(manifest.chunks.clone()),
        preview_limit: segments
            .first()
            .and_then(|segment| payloads.get(segment))
            .map_or(0, |metadata| metadata.preview_limit),
    };
    Some((reference, manifest, metadata))
}
