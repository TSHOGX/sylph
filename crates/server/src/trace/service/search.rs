use super::*;
use crate::trace::evidence::catalogue::{PayloadPin, PinOwnerKind};
use crate::trace::search::{SearchBody, SearchDocument, SearchMembership, SearchIndexWork, SearchStore};
use crate::trace::worker::ProvenanceMap;
use std::collections::BTreeSet;
use sylph_trace::reducer::{Reduction, Resolved};

#[cfg(test)]
pub(crate) struct FixtureIndexer {
    service: Arc<TraceService>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

#[cfg(test)]
impl FixtureIndexer {
    pub(crate) async fn index_pending(&self) -> anyhow::Result<SearchIndexWork> {
        self.service.index_pending_locked().await
    }
}

impl TraceService {
    /// Keep background indexing out of a fixture's publication/measurement window.
    #[cfg(test)]
    pub(crate) async fn fixture_indexer(self: &Arc<Self>) -> FixtureIndexer {
        FixtureIndexer {
            service: self.clone(),
            _guard: self.search_gate.clone().lock_owned().await,
        }
    }

    pub async fn search_store(&self, generation: &str) -> anyhow::Result<Arc<SearchStore>> {
        if let Some(store) = self.searches.read().await.get(generation) {
            return Ok(store.clone());
        }
        let mut searches = self.searches.write().await;
        if let Some(store) = searches.get(generation) {
            return Ok(store.clone());
        }
        let path = self.root.join(format!("search-{generation}.db"));
        let name = generation.to_owned();
        let limits = self.search_limits.clone();
        // The SQLx schema-opening future cannot move between request workers.
        // Cache hits remain asynchronous reads; only initialization crosses this boundary.
        let store = Arc::new(
            tokio::task::spawn_blocking(move || {
                tokio::runtime::Handle::current().block_on(SearchStore::open(&path, name, limits))
            })
            .await??,
        );
        searches.insert(generation.into(), store.clone());
        Ok(store)
    }

    pub(super) async fn retire_searches(&self, now_ms: i64) -> anyhow::Result<()> {
        let _guard = self.search_gate.lock().await;
        for generation in self.catalogue.retired_searches(now_ms, 8).await? {
            let index = self.search_store(&generation).await?;
            index.invalidate().await?;
            self.catalogue
                .release_revision_pin(PinOwnerKind::Search, &generation)
                .await?;
            let released = self
                .catalogue
                .release_owner_pins(PinOwnerKind::Search, &generation, 128)
                .await?;
            if released < 128 {
                self.searches.write().await.remove(&generation);
            }
        }
        Ok(())
    }

    pub async fn index_pending(&self) -> anyhow::Result<SearchIndexWork> {
        let _guard = self.search_gate.lock().await;
        self.index_pending_locked().await
    }

    async fn index_pending_locked(&self) -> anyhow::Result<SearchIndexWork> {
        self.require_available().await?;
        let now = crate::models::now_ms();
        let pending = self
            .catalogue
            .begin_lease("trace-search-index", now, 120_000)
            .await?;
        // The pending lease pins the generation before reading its checkpoint.
        // Install the durable index root before releasing that temporary bridge;
        // temporal reclamation can never pass through the handoff window.
        let prepared = async {
            let generation = self.generation(&pending.generation).await?;
            let (snapshot, reduction, provenance) =
                self.worker.projected_snapshot(&generation).await?;
            self.catalogue
                .finish_lease(&pending, &snapshot, now)
                .await?;
            let index = self.search_store(&snapshot.generation).await?;
            let update = if index.checkpoint().await?.as_ref() == Some(&snapshot) {
                None
            } else {
                // Invalidate visibility before moving the cross-store revision root.
                Some(index.begin_update(&snapshot).await?)
            };
            self.catalogue
                .pin_revision(PinOwnerKind::Search, &snapshot.generation, &snapshot)
                .await?;
            Ok::<_, anyhow::Error>((snapshot, reduction, provenance, index, update))
        }
        .await;
        self.catalogue
            .expire_lease("trace-search-index", &pending.lease_id)
            .await?;
        let (snapshot, reduction, provenance, index, update) = prepared?;
        let mut work = SearchIndexWork::default();
        let Some(update) = update else {
            return Ok(work);
        };
        let (sources, composed) = self.search_sources(&reduction, &provenance).await?;
        for source in sources {
            work.sources_examined += 1;
            let source_key = blake3::hash(&serde_json::to_vec(&source)?)
                .to_hex()
                .to_string();
            let reused = index.prepare_source(&update, &source_key).await?;
            let mut memberships = Vec::new();
            let membership_base = serde_json::to_vec(&(&source_key, &memberships))?.len();
            let mut membership_bytes = membership_base;
            for membership in &source.scopes {
                let size = serde_json::to_vec(membership)?.len() + 1;
                if memberships.len() >= index.limits().max_documents_per_batch
                    || (!memberships.is_empty()
                        && membership_bytes + size > index.limits().max_batch_bytes)
                {
                    index
                        .write_membership_batch(&update, &source_key, &memberships)
                        .await?;
                    memberships.clear();
                    membership_bytes = membership_base;
                }
                membership_bytes += size;
                memberships.push(membership.clone());
            }
            index
                .write_membership_batch(&update, &source_key, &memberships)
                .await?;
            index.complete_memberships(&update, &source_key).await?;
            if reused {
                work.sources_reused += 1;
                continue;
            }
            if let Some(payload) = composed.get(&source.part.payload) {
                self.stage_search_composed(&snapshot, &source, payload)
                    .await?;
            }
            // Each supporting recipient epoch is an independent retention root.
            // Original authorship does not add an origin-environment dependency.
            for epoch in &source.dependency_epochs {
                self.catalogue
                    .pin_payloads(
                        PinOwnerKind::Search,
                        &snapshot.generation,
                        &[PayloadPin {
                            payload: source.part.payload.clone(),
                            epoch: epoch.clone(),
                        }],
                    )
                    .await?;
            }
            let Some(length) = self.evidence.payload_length(&source.part.payload).await? else {
                return Err(anyhow::anyhow!("search source payload is unavailable"));
            };
            let mut documents = Vec::new();
            let batch_base = serde_json::to_vec(&(&source_key, &documents))?.len();
            let mut bytes = batch_base;
            let mut offset = 0u64;
            while offset < length {
                let amount =
                    (length - offset).min(index.limits().max_document_bytes as u64) as usize;
                let slice = self
                    .evidence
                    .payload_range(&source.part.payload, offset, amount)
                    .await?;
                work.payload_bytes_read += slice.bytes.len() as u64;
                let text = match std::str::from_utf8(&slice.bytes) {
                    Ok(text) => text,
                    Err(error)
                        if error.error_len().is_none() && offset + (amount as u64) < length =>
                    {
                        std::str::from_utf8(&slice.bytes[..error.valid_up_to()])?
                    }
                    Err(error) => return Err(error.into()),
                }
                .to_owned();
                let consumed = text.len();
                if consumed == 0 {
                    return Err(anyhow::anyhow!("search text cannot advance UTF-8 range"));
                }
                let key = blake3::hash(&serde_json::to_vec(&(&source_key, offset))?)
                    .to_hex()
                    .to_string();
                let document = SearchDocument {
                    key,
                    body: SearchBody {
                        media_type: source.part.media_type.clone(),
                        encoding: source.part.encoding,
                        total_bytes: length,
                        agent: source.agent.clone(),
                        anchor: AnyObjectRef::HistoryEntry(source.entry.clone()),
                        object: AnyObjectRef::Message(source.message.clone()),
                        payload: source.part.payload.clone(),
                        representation: source.representation,
                        body_revision: source.revision.encode(),
                        byte_start: offset,
                        ingestion_epoch: source.epoch.clone(),
                        dependency_epochs: source.dependency_epochs.clone(),
                    },
                    text,
                };
                let size = serde_json::to_vec(&document)?.len() + 1;
                if documents.len() >= index.limits().max_documents_per_batch
                    || (!documents.is_empty() && bytes + size > index.limits().max_batch_bytes)
                {
                    index
                        .write_source_batch(&update, &source_key, &documents)
                        .await?;
                    documents.clear();
                    bytes = batch_base;
                }
                bytes += size;
                documents.push(document);
                work.documents_written += 1;
                offset += consumed as u64;
                tokio::task::yield_now().await;
            }
            index
                .write_source_batch(&update, &source_key, &documents)
                .await?;
            index.complete_body(&update, &source_key).await?;
        }
        work.documents_removed = index.finish_update(&update).await?;
        Ok(work)
    }
}

fn known<'a>(reduction: &'a Reduction, reference: &AnyObjectRef) -> Option<&'a TraceObject> {
    reduction.object(reference)?.declaration.known()
}

#[derive(serde::Serialize)]
pub(super) struct IndexSource {
    agent: AgentRef,
    entry: EntryRef,
    revision: MessageRevisionRef,
    message: MessageRef,
    representation: Representation,
    part: ContentPart,
    epoch: IngestionEpoch,
    dependency_epochs: BTreeSet<IngestionEpoch>,
    // Membership changes have a separate publication path and cannot invalidate
    // an unchanged body's durable text cache.
    #[serde(skip)]
    scopes: Vec<SearchMembership>,
}

struct IndexVariant {
    agent: AgentRef,
    entry: EntryRef,
    revision: MessageRevisionRef,
    message: MessageRef,
    representation: Representation,
    epoch: IngestionEpoch,
    dependency_epochs: BTreeSet<IngestionEpoch>,
    scopes: Vec<SearchMembership>,
}

impl TraceService {
    pub(super) async fn search_sources(
        &self,
        reduction: &Reduction,
        provenance: &ProvenanceMap,
    ) -> anyhow::Result<(
        Vec<IndexSource>,
        BTreeMap<PayloadRef, sylph_trace::reading::ComposedPayload>,
    )> {
        let variants = source_variants(reduction, provenance)?;
        let mut metadata = sylph_trace::reading::PayloadPreviews::new();
        let requested = variants
            .iter()
            .map(|variant| variant.revision.clone())
            .collect();
        for payload in sylph_trace::reading::composition_payloads(reduction, &requested) {
            let length = self.evidence.payload_length(&payload).await?;
            // payload_manifest verifies its stored bytes. Calling it for a raw
            // Bytes handle would reread every unchanged body merely for metadata.
            let manifest = if payload.kind() == PayloadKind::Manifest {
                self.evidence.payload_manifest(&payload).await?
            } else {
                None
            };
            let available = length.is_some()
                && manifest
                    .as_ref()
                    .is_none_or(|manifest| manifest.completeness == PayloadCompleteness::Complete);
            metadata.insert(
                payload.clone(),
                Arc::new(sylph_trace::reading::PayloadPreview {
                    total_bytes: length,
                    availability: Some(if available {
                        ReadAvailability::Available
                    } else {
                        ReadAvailability::Unavailable {
                            reason: "Retained append payload is incomplete".into(),
                        }
                    }),
                    chunks: manifest.map(|manifest| manifest.chunks),
                    ..Default::default()
                }),
            );
        }
        Ok(compose_sources(reduction, variants, &mut metadata))
    }

    async fn stage_search_composed(
        &self,
        snapshot: &super::super::generation::Snapshot,
        source: &IndexSource,
        composed: &sylph_trace::reading::ComposedPayload,
    ) -> anyhow::Result<()> {
        let upload = self
            .catalogue
            .begin_upload(
                PinOwnerKind::Search,
                &snapshot.generation,
                source.part.payload.clone(),
                source.epoch.clone(),
                crate::models::now_ms(),
            )
            .await?;
        if self.evidence.stage_manifest(&composed.manifest).await? != source.part.payload {
            anyhow::bail!("search composed payload digest mismatch");
        }
        self.catalogue.finish_upload(&upload).await?;
        for epoch in source
            .dependency_epochs
            .iter()
            .filter(|epoch| *epoch != &source.epoch)
        {
            self.catalogue
                .pin_payloads(
                    PinOwnerKind::Search,
                    &snapshot.generation,
                    &[PayloadPin {
                        payload: source.part.payload.clone(),
                        epoch: epoch.clone(),
                    }],
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
fn sources(reduction: &Reduction, provenance: &ProvenanceMap) -> anyhow::Result<Vec<IndexSource>> {
    Ok(compose_sources(
        reduction,
        source_variants(reduction, provenance)?,
        &mut sylph_trace::reading::PayloadPreviews::new(),
    )
    .0)
}

fn locally_authored(
    reduction: &Reduction,
    entry: &HistoryEntry,
    reference: &MessageRevisionRef,
) -> bool {
    if entry.original_owner.is_some() {
        return false;
    }
    let Some(TraceObject::MessageRevision(revision)) =
        known(reduction, &AnyObjectRef::MessageRevision(reference.clone()))
    else {
        return false;
    };
    let Some(message) = sylph_trace::reading::message_metadata(reduction, &revision.message) else {
        return false;
    };
    matches!(
        &message.author,
        Author::Agent { agent: OwnerRef::Agent { agent } } if agent == &entry.containing_agent
    ) && matches!(
        &message.production,
        Some(ProductionScope { owner: OwnerRef::Agent { agent }, .. })
            if agent == &entry.containing_agent
    )
}

fn source_variants(
    reduction: &Reduction,
    provenance: &ProvenanceMap,
) -> anyhow::Result<Vec<IndexVariant>> {
    let mut result = Vec::new();
    let mut selections = BTreeMap::<AgentRef, Vec<(&SelectionRef, &Vec<EntryRef>)>>::new();
    for (reference, membership) in &reduction.selection_memberships {
        if let (Some(entries), Some(TraceObject::HistorySelection(selection))) = (
            membership.as_ref(),
            known(
                reduction,
                &AnyObjectRef::HistorySelection(reference.clone()),
            ),
        ) {
            selections
                .entry(selection.agent.clone())
                .or_default()
                .push((reference, entries));
        }
    }
    for (agent, history) in &reduction.histories {
        let mut variants = BTreeMap::<(EntryRef, MessageRevisionRef), Vec<SearchMembership>>::new();
        for entry_ref in &history.archive_order {
            let Some(TraceObject::HistoryEntry(entry)) =
                known(reduction, &AnyObjectRef::HistoryEntry(entry_ref.clone()))
            else {
                continue;
            };

            for revision in bound_revisions(reduction, entry_ref, None) {
                let mut scopes = vec![SearchMembership::RetainedArchive];
                if locally_authored(reduction, entry, &revision) {
                    scopes.push(SearchMembership::Authored);
                }
                variants.insert((entry_ref.clone(), revision), scopes);
            }
        }
        // Entry membership cannot authorize every body observed for that identity.
        // Resolve the selected bindings independently; archive variants stay archived.
        for &(selection, entries) in selections.get(agent).into_iter().flatten() {
            let mut messages =
                BTreeMap::<MessageRef, (EntryRef, BTreeSet<MessageRevisionRef>)>::new();
            for entry in entries {
                for reference in bound_revisions(reduction, entry, Some(selection)) {
                    let Some(TraceObject::MessageRevision(revision)) =
                        known(reduction, &AnyObjectRef::MessageRevision(reference.clone()))
                    else {
                        continue;
                    };
                    messages
                        .entry(revision.message.clone())
                        .or_insert_with(|| (entry.clone(), BTreeSet::new()))
                        .1
                        .insert(reference);
                }
            }
            for (message, (anchor, included)) in messages {
                if let Some(revision) = reduction.select_message_in(&message, &included).known() {
                    variants
                        .entry((anchor, revision.clone()))
                        .or_default()
                        .push(SearchMembership::SelectedContext(selection.clone()));
                }
            }
        }
        for ((entry_ref, revision_ref), scopes) in variants {
            let Some(TraceObject::HistoryEntry(_)) =
                known(reduction, &AnyObjectRef::HistoryEntry(entry_ref.clone()))
            else {
                continue;
            };
            let Some(TraceObject::MessageRevision(revision)) = known(
                reduction,
                &AnyObjectRef::MessageRevision(revision_ref.clone()),
            ) else {
                continue;
            };
            let resolved_entry = reduction
                .object(&AnyObjectRef::HistoryEntry(entry_ref.clone()))
                .ok_or_else(|| anyhow::anyhow!("search entry lacks its resolved declaration"))?;
            let support = provenance
                .get(&resolved_entry.reference)
                .ok_or_else(|| anyhow::anyhow!("search entry lacks evidence provenance"))?;

            let mut dependency_epochs: BTreeSet<_> =
                support.dependency_epochs.iter().cloned().collect();
            // The entry binds a representation, while its bytes and authored
            // fields have their own retained support. Follow only the actual
            // append chain; author/original-owner links do not own copied bytes.
            let mut pending = vec![
                AnyObjectRef::MessageRevision(revision_ref.clone()),
                AnyObjectRef::Message(revision.message.clone()),
            ];
            let mut visited = BTreeSet::new();
            // A chain link whose declaration or retained support is unavailable
            // leaves part of the body unbacked. Indexing the variant anyway would
            // expose those trailing bytes without their evidence, so drop it.
            let mut supported = true;
            while let Some(reference) = pending.pop() {
                let Some(object) = reduction.object(&reference) else {
                    supported = false;
                    break;
                };
                if !visited.insert(object.reference.clone()) {
                    continue;
                }
                let Some(contribution) = provenance.get(&object.reference) else {
                    supported = false;
                    break;
                };
                dependency_epochs.extend(contribution.dependency_epochs.iter().cloned());
                if let Some(TraceObject::MessageRevision(MessageRevision {
                    form:
                        RevisionForm::Append {
                            predecessor: Some(predecessor),
                        },
                    ..
                })) = object.declaration.known()
                {
                    pending.push(AnyObjectRef::MessageRevision(predecessor.clone()));
                }
            }
            if !supported {
                continue;
            }
            result.push(IndexVariant {
                agent: agent.clone(),
                entry: entry_ref,
                revision: revision_ref,
                message: revision.message.clone(),
                representation: revision.representation,
                epoch: support.primary_epoch.clone(),
                dependency_epochs,
                scopes,
            });
        }
    }
    Ok(result)
}

fn compose_sources(
    reduction: &Reduction,
    variants: Vec<IndexVariant>,
    metadata: &mut sylph_trace::reading::PayloadPreviews,
) -> (
    Vec<IndexSource>,
    BTreeMap<PayloadRef, sylph_trace::reading::ComposedPayload>,
) {
    let requested = variants
        .iter()
        .map(|variant| variant.revision.clone())
        .collect();
    let composition = sylph_trace::reading::compose_revisions(reduction, &requested, metadata);
    let mut sources = Vec::new();
    for variant in variants {
        if composition.unavailable.contains_key(&variant.revision) {
            continue;
        }
        let Some(TraceObject::MessageRevision(revision)) = known(
            reduction,
            &AnyObjectRef::MessageRevision(variant.revision.clone()),
        ) else {
            continue;
        };
        let parts = composition
            .parts
            .get(&variant.revision)
            .unwrap_or(&revision.parts);
        for part in parts {
            if matches!(
                part.kind,
                ContentKind::Text | ContentKind::DisclosedReasoning | ContentKind::Structured
            ) && part.encoding == PayloadEncoding::Utf8
            {
                sources.push(IndexSource {
                    agent: variant.agent.clone(),
                    entry: variant.entry.clone(),
                    revision: variant.revision.clone(),
                    message: variant.message.clone(),
                    representation: variant.representation,
                    part: part.clone(),
                    epoch: variant.epoch.clone(),
                    dependency_epochs: variant.dependency_epochs.clone(),
                    scopes: variant.scopes.clone(),
                });
            }
        }
    }
    (sources, composition.payloads)
}

fn bound_revisions(
    reduction: &Reduction,
    entry: &EntryRef,
    selection: Option<&SelectionRef>,
) -> BTreeSet<MessageRevisionRef> {
    reduction
        .entry_contents(entry, selection)
        .slots
        .values()
        .filter_map(Resolved::known)
        .flatten()
        .filter_map(|content| match content {
            HistoryContent::Message(reference) => Some(reference.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::worker::ObjectProvenance;
    use sylph_trace::reducer::Reducer;

    fn agent() -> AgentRef {
        AgentRef::Session(sylph_protocol::SessionId::decode("env7:codex:search-bindings").unwrap())
    }

    fn position(index: u64) -> SourcePosition {
        SourcePosition {
            source: SourceRef {
                issuer: Issuer::Server("fixture".into()),
                instance: "observations".into(),
            },
            generation: 0,
            locator: SourceLocator::Frame { ordinal: index },
        }
    }

    fn reference(index: u64) -> ClaimRef {
        ClaimRef::occurrence(agent(), position(index), vec!["fixture".into()]).unwrap()
    }

    fn entry_provenance(reduction: &Reduction, epochs: &[IngestionEpoch]) -> ProvenanceMap {
        reduction
            .objects
            .iter()
            .filter_map(|(reference, object)| {
                let mut claims: Vec<_> = match &object.declaration {
                    Resolved::Known { claims, .. } => claims.clone(),
                    Resolved::Conflicted { alternatives } => alternatives
                        .iter()
                        .flat_map(|(_, claims)| claims.iter().cloned())
                        .collect(),
                    _ => return None,
                };
                claims.sort();
                claims.dedup();
                Some((
                    reference.clone(),
                    Arc::new(ObjectProvenance {
                        primary_claim: claims[0].clone(),
                        primary_observation: ObservationRef {
                            issuer: Issuer::Server("fixture".into()),
                            producer_id: "entry-provenance".into(),
                            producer_epoch: "one".into(),
                            producer_seq: 1,
                        },
                        primary_epoch: epochs[0].clone(),
                        dependency_epochs: epochs.iter().cloned().collect(),
                    }),
                ))
            })
            .collect()
    }

    struct StreamFixture {
        claims: Vec<Claim>,
        payloads: BTreeMap<PayloadRef, Vec<u8>>,
        revisions: Vec<MessageRevisionRef>,
        entries: Vec<EntryRef>,
        selection: SelectionRef,
    }

    impl StreamFixture {
        fn new(selected: usize) -> Self {
            let message = MessageRef::native(agent(), "stream", vec!["message".into()]).unwrap();
            let part = MessagePartRef::native(agent(), "stream", vec!["part".into()]).unwrap();
            let selection =
                SelectionRef::native(agent(), "stream", vec!["selected".into()]).unwrap();
            let revisions = (0..6)
                .map(|index| {
                    MessageRevisionRef::occurrence(agent(), position(index), vec!["stream".into()])
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let entries = (0..6)
                .map(|index| EntryRef::native(agent(), "stream", vec![index.to_string()]).unwrap())
                .collect::<Vec<_>>();
            let mut facts = vec![
                Fact::Declare {
                    object: TraceObject::Agent(AgentThread {
                        agent_ref: agent(),
                        vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
                        name: None,
                        model: None,
                    }),
                },
                Fact::Declare {
                    object: TraceObject::Message(Message {
                        message_ref: message.clone(),
                        author: Author::Unknown,
                        protocol_role: None,
                        purpose: MessagePurpose::Unclassified,
                        production: None,
                    }),
                },
            ];
            let mut payloads = BTreeMap::new();
            for (index, body) in ["ALPHA ", "BETA ", "BETA ", "RESET ", "VALUE", "FINAL"]
                .into_iter()
                .enumerate()
            {
                let payload = PayloadRef::bytes(blake3::hash(body.as_bytes()).to_hex()).unwrap();
                payloads.insert(payload.clone(), body.as_bytes().to_vec());
                let form = match index {
                    0 => RevisionForm::Append { predecessor: None },
                    3 => RevisionForm::Replacement { covers: None },
                    5 => RevisionForm::Final { covers: None },
                    _ => RevisionForm::Append {
                        predecessor: Some(revisions[index - 1].clone()),
                    },
                };
                facts.push(Fact::Declare {
                    object: TraceObject::HistoryEntry(HistoryEntry {
                        entry_ref: entries[index].clone(),
                        containing_agent: agent(),
                        predecessor: index.checked_sub(1).map(|prior| entries[prior].clone()),
                        original_owner: None,
                    }),
                });
                facts.push(Fact::Declare {
                    object: TraceObject::MessageRevision(MessageRevision {
                        revision_ref: revisions[index].clone(),
                        message: message.clone(),
                        representation: Representation::Conversation,
                        form,
                        parts: vec![ContentPart {
                            part_ref: part.clone(),
                            kind: ContentKind::Text,
                            payload,
                            media_type: Some("text/plain".into()),
                            encoding: PayloadEncoding::Utf8,
                        }],
                    }),
                });
                facts.push(Fact::HistoryContentObserved {
                    observation: HistoryContentObservation::complete(
                        entries[index].clone(),
                        position(index as u64),
                        "fixture/stream",
                        vec![HistoryContent::Message(revisions[index].clone())],
                    ),
                });
            }
            facts.push(Fact::Declare {
                object: TraceObject::HistorySelection(HistorySelection {
                    selection_ref: selection.clone(),
                    agent: agent(),
                    membership: SelectionMembership::EntrySet {
                        entries: entries.clone(),
                    },
                    content_scope: HistoryContentScope::ObservationSet {
                        observations: vec![reference((4 + selected * 3) as u64)],
                    },
                    excluded_heads: vec![],
                }),
            });
            facts.push(Fact::SelectHistory {
                agent: agent(),
                selection: selection.clone(),
                selected_at: position(100),
            });
            let claims = facts
                .into_iter()
                .enumerate()
                .map(|(index, fact)| Claim {
                    claim_ref: reference(index as u64),
                    fact,
                    evidence: vec![ObservationRef {
                        issuer: Issuer::Server("fixture".into()),
                        producer_id: "stream".into(),
                        producer_epoch: "one".into(),
                        producer_seq: index as u64 + 1,
                    }],
                    rule_id: "fixture/stream".into(),
                    mapper_build: "test".into(),
                    supersedes: vec![],
                    source_revision: None,
                })
                .collect();
            Self {
                claims,
                payloads,
                revisions,
                entries,
                selection,
            }
        }

        fn reduction(&self) -> Reduction {
            let mut reducer = Reducer::default();
            reducer
                .apply(
                    &self.claims,
                    &self
                        .claims
                        .iter()
                        .flat_map(|claim| claim.evidence.clone())
                        .collect(),
                    self.claims.len(),
                )
                .unwrap();
            reducer.resolve()
        }

        fn metadata(&self) -> sylph_trace::reading::PayloadPreviews {
            self.payloads
                .iter()
                .map(|(payload, bytes)| {
                    (
                        payload.clone(),
                        Arc::new(sylph_trace::reading::PayloadPreview {
                            total_bytes: Some(bytes.len() as u64),
                            availability: Some(ReadAvailability::Available),
                            ..Default::default()
                        }),
                    )
                })
                .collect()
        }

        fn sources(
            &self,
            metadata: &mut sylph_trace::reading::PayloadPreviews,
        ) -> (
            Vec<IndexSource>,
            BTreeMap<PayloadRef, sylph_trace::reading::ComposedPayload>,
        ) {
            let reduction = self.reduction();
            let provenance =
                entry_provenance(&reduction, &[IngestionEpoch::new("stream-epoch").unwrap()]);
            compose_sources(
                &reduction,
                source_variants(&reduction, &provenance).unwrap(),
                metadata,
            )
        }

        fn body(
            &self,
            source: &IndexSource,
            composed: &BTreeMap<PayloadRef, sylph_trace::reading::ComposedPayload>,
        ) -> Vec<u8> {
            match composed.get(&source.part.payload) {
                None => self.payloads[&source.part.payload].clone(),
                Some(composed) => composed
                    .manifest
                    .chunks
                    .iter()
                    .flat_map(|chunk| {
                        let bytes = &self.payloads[&chunk.digest];
                        bytes[chunk.chunk_offset as usize
                            ..(chunk.chunk_offset + chunk.length) as usize]
                            .iter()
                            .copied()
                    })
                    .collect(),
            }
        }
    }

    #[test]
    fn unknown_or_inherited_production_never_enters_locally_authored_search() {
        let origin =
            AgentRef::Session(sylph_protocol::SessionId::decode("private:codex:origin").unwrap());
        let local_author = Author::Agent {
            agent: OwnerRef::Agent { agent: agent() },
        };
        let local_production = Some(ProductionScope {
            owner: OwnerRef::Agent { agent: agent() },
            activation: None,
        });
        for (original, author, production) in [
            (Some(OwnerRef::Unknown), Author::Unknown, None),
            (
                Some(OwnerRef::Unresolved {
                    scope: agent(),
                    namespace: "fork".into(),
                    native_key: vec!["entry".into()],
                    reason: "Unproved prefix".into(),
                }),
                Author::Unknown,
                None,
            ),
            (None, Author::Human { subject: None }, None),
            (
                None,
                Author::Agent {
                    agent: OwnerRef::Agent { agent: origin },
                },
                None,
            ),
            (
                None,
                Author::Agent {
                    agent: OwnerRef::Agent { agent: agent() },
                },
                None,
            ),
            (
                Some(OwnerRef::Unknown),
                local_author.clone(),
                local_production.clone(),
            ),
            (
                Some(OwnerRef::Agent { agent: agent() }),
                local_author.clone(),
                local_production.clone(),
            ),
            (None, Author::Unknown, local_production.clone()),
            (
                None,
                local_author,
                Some(ProductionScope {
                    owner: OwnerRef::Unknown,
                    activation: None,
                }),
            ),
        ] {
            let mut fixture = StreamFixture::new(2);
            for claim in &mut fixture.claims {
                match &mut claim.fact {
                    Fact::Declare {
                        object: TraceObject::HistoryEntry(entry),
                    } => entry.original_owner = original.clone(),
                    Fact::Declare {
                        object: TraceObject::Message(message),
                    } => {
                        message.author = author.clone();
                        message.production = production.clone();
                    }
                    _ => {}
                }
            }
            let reduction = fixture.reduction();
            let epochs = [IngestionEpoch::new("retained").unwrap()];
            let variants =
                source_variants(&reduction, &entry_provenance(&reduction, &epochs)).unwrap();
            assert!(
                !variants.is_empty(),
                "unknown ownership cannot hide retained bytes"
            );
            assert!(
                variants
                    .iter()
                    .all(|row| !row.scopes.contains(&SearchMembership::Authored)),
                "containing identity became authored-search evidence"
            );
        }
    }

    #[test]
    fn agreed_local_authorship_survives_unrelated_metadata_conflict() {
        let mut fixture = StreamFixture::new(2);
        let mut extra = None;
        for claim in &mut fixture.claims {
            if let Fact::Declare {
                object: TraceObject::Message(message),
            } = &mut claim.fact
            {
                message.author = Author::Agent {
                    agent: OwnerRef::Agent { agent: agent() },
                };
                message.production = Some(ProductionScope {
                    owner: OwnerRef::Agent { agent: agent() },
                    activation: None,
                });
                message.purpose = MessagePurpose::Progress;
                let mut other = claim.clone();
                other.claim_ref = reference(999);
                if let Fact::Declare {
                    object: TraceObject::Message(message),
                } = &mut other.fact
                {
                    message.purpose = MessagePurpose::Answer;
                }
                extra = Some(other);
            }
        }
        fixture.claims.push(extra.unwrap());
        let reduction = fixture.reduction();
        assert!(reduction.objects.values().any(|object| matches!(
            &object.declaration,
            Resolved::Conflicted { alternatives }
                if alternatives.iter().all(|(value, _)| matches!(value, TraceObject::Message(_)))
        )));
        let epochs = [IngestionEpoch::new("local").unwrap()];
        let variants = source_variants(&reduction, &entry_provenance(&reduction, &epochs)).unwrap();
        assert!(!variants.is_empty());
        assert!(
            variants
                .iter()
                .all(|row| { row.scopes.contains(&SearchMembership::Authored) })
        );
    }

    #[test]
    fn copied_search_content_uses_recipient_support_without_origin_provenance() {
        let mut fixture = StreamFixture::new(2);
        let origin =
            AgentRef::Session(sylph_protocol::SessionId::decode("private:codex:origin").unwrap());
        for claim in &mut fixture.claims {
            if let Fact::Declare {
                object: TraceObject::HistoryEntry(entry),
            } = &mut claim.fact
            {
                entry.original_owner = Some(OwnerRef::Agent {
                    agent: origin.clone(),
                });
            }
        }
        let reduction = fixture.reduction();
        let epochs = ["recipient-primary", "recipient-support"]
            .map(|value| IngestionEpoch::new(value).unwrap());
        let provenance = entry_provenance(&reduction, &epochs);
        let variants = source_variants(&reduction, &provenance).unwrap();
        assert_eq!(variants.len(), fixture.entries.len());
        for source in variants {
            assert_eq!(source.agent, agent());
            assert_eq!(source.epoch, epochs[0]);
            assert_eq!(source.dependency_epochs, BTreeSet::from(epochs.clone()));
            assert!(!source.scopes.contains(&SearchMembership::Authored));
        }
    }

    #[test]
    fn search_composes_repeated_appends_and_keeps_replacement_and_final_bodies_independent() {
        for selected in [2, 4, 5] {
            let fixture = StreamFixture::new(selected);
            let (sources, composed) = fixture.sources(&mut fixture.metadata());
            let expected = [
                "ALPHA ",
                "ALPHA BETA ",
                "ALPHA BETA BETA ",
                "RESET ",
                "RESET VALUE",
                "FINAL",
            ];
            for (index, expected) in expected.iter().enumerate() {
                let source = sources
                    .iter()
                    .find(|source| {
                        source.revision == fixture.revisions[index]
                            && source.scopes.contains(&SearchMembership::RetainedArchive)
                    })
                    .unwrap();
                assert_eq!(source.entry, fixture.entries[index]);
                assert_eq!(fixture.body(source, &composed), expected.as_bytes());
            }
            let selected_sources = sources
                .iter()
                .filter(|source| {
                    source.scopes.contains(&SearchMembership::SelectedContext(
                        fixture.selection.clone(),
                    ))
                })
                .collect::<Vec<_>>();
            assert_eq!(selected_sources.len(), 1);
            assert_eq!(selected_sources[0].revision, fixture.revisions[selected]);
            assert_eq!(
                fixture.body(selected_sources[0], &composed),
                expected[selected].as_bytes()
            );
            assert!(
                sources
                    .iter()
                    .all(|source| serde_json::to_vec(source).is_ok())
            );
        }
    }

    #[test]
    fn search_never_indexes_a_trailing_fragment_when_append_evidence_is_unavailable() {
        let fixture = StreamFixture::new(2);
        let mut missing_metadata = fixture.metadata();
        let first = PayloadRef::bytes(blake3::hash(b"ALPHA ").to_hex()).unwrap();
        missing_metadata.remove(&first);
        let (sources, _) = fixture.sources(&mut missing_metadata);
        for index in [1, 2] {
            assert!(
                !sources
                    .iter()
                    .any(|source| source.revision == fixture.revisions[index])
            );
        }
        assert!(!sources.iter().any(|source| {
            source
                .scopes
                .iter()
                .any(|membership| matches!(membership, SearchMembership::SelectedContext(_)))
        }));

        let mut missing_predecessor = StreamFixture::new(2);
        missing_predecessor.claims.retain(|claim| {
            !matches!(&claim.fact,
            Fact::Declare { object: TraceObject::MessageRevision(revision) }
                if revision.revision_ref == fixture.revisions[0])
        });
        let (sources, _) = missing_predecessor.sources(&mut missing_predecessor.metadata());
        assert!(
            !sources
                .iter()
                .any(|source| source.revision == fixture.revisions[1]
                    || source.revision == fixture.revisions[2])
        );
        assert!(
            sources
                .iter()
                .any(|source| source.revision == fixture.revisions[4])
        );
    }

    #[tokio::test]
    async fn composed_search_sources_stage_exact_manifest_bytes_and_epoch_roots_on_demand() {
        let directory = tempfile::tempdir().unwrap();
        let service = TraceService::open(directory.path()).await.unwrap();
        let fixture = StreamFixture::new(2);
        for (reference, bytes) in &fixture.payloads {
            assert_eq!(
                &service.evidence.stage_payload(bytes).await.unwrap(),
                reference
            );
        }
        let epoch = service
            .evidence
            .authorize_scope(&IngestionScope::Agent(agent()), &[agent()])
            .await
            .unwrap();
        let supporting_agent = AgentRef::Session(
            sylph_protocol::SessionId::decode("env7:codex:search-support").unwrap(),
        );
        let supporting_epoch = service
            .evidence
            .authorize_scope(
                &IngestionScope::Agent(supporting_agent.clone()),
                &[supporting_agent],
            )
            .await
            .unwrap();
        let generation = service.active().await.unwrap();
        let snapshot = generation.checkpoint().await.unwrap().snapshot;
        let reduction = fixture.reduction();
        let provenance = entry_provenance(&reduction, &[epoch.clone(), supporting_epoch.clone()]);
        let (sources, composed) = service
            .search_sources(&reduction, &provenance)
            .await
            .unwrap();
        assert!(
            service
                .catalogue
                .payload_pins(PinOwnerKind::Search, &snapshot.generation, None, 10)
                .await
                .unwrap()
                .is_empty()
        );
        let source = sources
            .iter()
            .find(|source| source.revision == fixture.revisions[2])
            .unwrap();
        service
            .stage_search_composed(&snapshot, source, &composed[&source.part.payload])
            .await
            .unwrap();
        assert_eq!(
            service
                .evidence
                .payload_bytes(&source.part.payload, 1024)
                .await
                .unwrap(),
            b"ALPHA BETA BETA "
        );
        let pins = service
            .catalogue
            .payload_pins(PinOwnerKind::Search, &snapshot.generation, None, 10)
            .await
            .unwrap();
        assert_eq!(pins.len(), 2);
        assert!(pins.iter().all(|pin| pin.payload == source.part.payload));
        assert_eq!(
            pins.into_iter()
                .map(|pin| pin.epoch)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([epoch, supporting_epoch])
        );
        drop(generation);
        super::super::tests::close(service).await;
    }

    #[test]
    fn selected_search_uses_bound_revisions_while_archive_search_keeps_other_observations() {
        let entry = EntryRef::native(agent(), "entry", vec!["same-entry".into()]).unwrap();
        let message = MessageRef::native(agent(), "message", vec!["same-message".into()]).unwrap();
        let selection =
            SelectionRef::native(agent(), "selection", vec!["source-one".into()]).unwrap();
        let revisions = ["one", "two"]
            .map(|key| MessageRevisionRef::native(agent(), "revision", vec![key.into()]).unwrap());
        for observations in [vec![reference(4)], vec![]] {
            let mut facts = vec![
                Fact::Declare {
                    object: TraceObject::Agent(AgentThread {
                        agent_ref: agent(),
                        vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
                        name: None,
                        model: None,
                    }),
                },
                Fact::Declare {
                    object: TraceObject::Message(Message {
                        message_ref: message.clone(),
                        author: Author::Unknown,
                        protocol_role: None,
                        purpose: MessagePurpose::Unclassified,
                        production: None,
                    }),
                },
                Fact::Declare {
                    object: TraceObject::HistoryEntry(HistoryEntry {
                        entry_ref: entry.clone(),
                        containing_agent: agent(),
                        predecessor: None,
                        original_owner: None,
                    }),
                },
            ];
            for (index, revision) in revisions.iter().enumerate() {
                facts.push(Fact::Declare {
                    object: TraceObject::MessageRevision(MessageRevision {
                        revision_ref: revision.clone(),
                        message: message.clone(),
                        representation: Representation::Conversation,
                        form: RevisionForm::Complete,
                        parts: vec![ContentPart {
                            part_ref: MessagePartRef::native(
                                agent(),
                                "part",
                                vec![index.to_string()],
                            )
                            .unwrap(),
                            kind: ContentKind::Text,
                            payload: PayloadRef::bytes(
                                if index == 0 { "a" } else { "b" }.repeat(64),
                            )
                            .unwrap(),
                            media_type: Some("text/plain".into()),
                            encoding: PayloadEncoding::Utf8,
                        }],
                    }),
                });
                facts.push(Fact::HistoryContentObserved {
                    observation: HistoryContentObservation::complete(
                        entry.clone(),
                        position(index as u64),
                        "fixture",
                        vec![HistoryContent::Message(revision.clone())],
                    ),
                });
            }
            facts.push(Fact::Declare {
                object: TraceObject::HistorySelection(HistorySelection {
                    selection_ref: selection.clone(),
                    agent: agent(),
                    membership: SelectionMembership::EntrySet {
                        entries: vec![entry.clone()],
                    },
                    content_scope: HistoryContentScope::ObservationSet {
                        observations: observations.clone(),
                    },
                    excluded_heads: vec![],
                }),
            });
            facts.push(Fact::SelectHistory {
                agent: agent(),
                selection: selection.clone(),
                selected_at: position(8),
            });
            let claims: Vec<_> = facts
                .into_iter()
                .enumerate()
                .map(|(index, fact)| Claim {
                    claim_ref: reference(index as u64),
                    fact,
                    evidence: vec![ObservationRef {
                        issuer: Issuer::Server("fixture".into()),
                        producer_id: "source".into(),
                        producer_epoch: "epoch".into(),
                        producer_seq: index as u64 + 1,
                    }],
                    rule_id: "fixture".into(),
                    mapper_build: "test".into(),
                    supersedes: vec![],
                    source_revision: None,
                })
                .collect();
            let mut reducer = Reducer::default();
            reducer
                .apply(
                    &claims,
                    &claims
                        .iter()
                        .flat_map(|claim| claim.evidence.clone())
                        .collect(),
                    100,
                )
                .unwrap();
            let reduction = reducer.resolve();
            let sources = sources(
                &reduction,
                &entry_provenance(&reduction, &[IngestionEpoch::new("epoch").unwrap()]),
            )
            .unwrap();
            let archived: BTreeSet<_> = sources
                .iter()
                .filter(|source| source.scopes.contains(&SearchMembership::RetainedArchive))
                .map(|source| source.revision.clone())
                .collect();
            assert_eq!(archived, BTreeSet::from(revisions.clone()));
            let selected: Vec<_> = sources
                .iter()
                .filter(|source| {
                    source
                        .scopes
                        .contains(&SearchMembership::SelectedContext(selection.clone()))
                })
                .collect();
            if observations.is_empty() {
                assert!(selected.is_empty());
            } else {
                assert_eq!(selected.len(), 1);
                assert_eq!(selected[0].revision, revisions[0]);
                assert_eq!(selected[0].entry, entry);
            }
        }
    }
}
