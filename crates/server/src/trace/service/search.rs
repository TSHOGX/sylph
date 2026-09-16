use super::*;
use crate::trace::evidence::catalogue::{PayloadPin, PinOwnerKind};
use crate::trace::search::{SearchDocument, SearchHistoryMode, SearchStore};
use sylph_trace::reducer::Reduction;

impl TraceService {
    pub async fn search_store(&self, generation: &str) -> anyhow::Result<Arc<SearchStore>> {
        if let Some(store) = self.searches.read().await.get(generation) {
            return Ok(store.clone());
        }
        let mut searches = self.searches.write().await;
        if let Some(store) = searches.get(generation) {
            return Ok(store.clone());
        }
        let store = Arc::new(
            SearchStore::open(
                &self.root.join(format!("search-{generation}.db")),
                generation.into(),
                self.search_limits.clone(),
            )
            .await?,
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

    pub async fn index_pending(&self) -> anyhow::Result<()> {
        let _guard = self.search_gate.lock().await;
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
            let (snapshot, reduction, epochs) = self.worker.projected_snapshot(&generation).await?;
            self.catalogue
                .finish_lease(&pending, &snapshot, now)
                .await?;
            let index = self.search_store(&snapshot.generation).await?;
            let unchanged = index.checkpoint().await?.as_ref() == Some(&snapshot);
            if !unchanged {
                index.begin_reindex().await?;
            }
            self.catalogue
                .pin_revision(PinOwnerKind::Search, &snapshot.generation, &snapshot)
                .await?;
            Ok::<_, anyhow::Error>((snapshot, reduction, epochs, index, unchanged))
        }
        .await;
        self.catalogue
            .expire_lease("trace-search-index", &pending.lease_id)
            .await?;
        let (snapshot, reduction, epochs, index, unchanged) = prepared?;
        if unchanged {
            return Ok(());
        }
        let mut expected = index.revision().await?;
        let mut documents = Vec::new();
        let mut bytes = 7usize;
        for source in sources(&reduction, &epochs)? {
            self.catalogue
                .pin_payloads(
                    PinOwnerKind::Search,
                    &snapshot.generation,
                    &[PayloadPin {
                        payload: source.part.payload.clone(),
                        epoch: source.epoch.clone(),
                    }],
                )
                .await?;
            let Some(length) = self.evidence.payload_length(&source.part.payload).await? else {
                continue;
            };
            let mut offset = 0u64;
            while offset < length {
                let amount =
                    (length - offset).min(index.limits().max_document_bytes as u64) as usize;
                let slice = self
                    .evidence
                    .payload_range(&source.part.payload, offset, amount)
                    .await?;
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
                for (mode, selection) in source.scopes.clone() {
                    let key = blake3::hash(&serde_json::to_vec(&(
                        &source.agent,
                        &source.entry,
                        &source.revision,
                        &source.part.part_ref,
                        offset,
                        &mode,
                        &selection,
                    ))?)
                    .to_hex()
                    .to_string();
                    let document = SearchDocument {
                        key,
                        agent: source.agent.clone(),
                        original_owner: source.original_owner.clone(),
                        anchor: AnyObjectRef::HistoryEntry(source.entry.clone()),
                        object: AnyObjectRef::Message(source.message.clone()),
                        selection,
                        mode,
                        payload: source.part.payload.clone(),
                        representation: source.representation,
                        body_revision: source.revision.encode(),
                        byte_start: offset,
                        text: text.clone(),
                        ingestion_epoch: source.epoch.clone(),
                    };
                    let size = serde_json::to_vec(&document)?.len() + 1;
                    if !documents.is_empty()
                        && (documents.len() == index.limits().max_documents_per_batch
                            || bytes + size > index.limits().max_batch_bytes)
                    {
                        index
                            .apply(expected, &snapshot, &documents, &[], false)
                            .await?;
                        expected = snapshot.view_revision;
                        documents.clear();
                        bytes = 7;
                    }
                    bytes += size;
                    documents.push(document);
                }
                offset += consumed as u64;
                tokio::task::yield_now().await;
            }
        }
        index
            .apply(expected, &snapshot, &documents, &[], true)
            .await?;
        Ok(())
    }
}

fn known<'a>(reduction: &'a Reduction, reference: &AnyObjectRef) -> Option<&'a TraceObject> {
    reduction.object(reference)?.declaration.known()
}

struct IndexSource {
    agent: AgentRef,
    original_owner: AgentRef,
    entry: EntryRef,
    revision: MessageRevisionRef,
    message: MessageRef,
    representation: Representation,
    part: ContentPart,
    epoch: IngestionEpoch,
    scopes: Vec<(SearchHistoryMode, Option<SelectionRef>)>,
}
fn sources(
    reduction: &Reduction,
    epochs: &BTreeMap<AnyObjectRef, IngestionEpoch>,
) -> anyhow::Result<Vec<IndexSource>> {
    let mut result = Vec::new();
    for (agent, history) in &reduction.histories {
        let selected: std::collections::BTreeSet<_> = history
            .selected_entries
            .as_ref()
            .into_iter()
            .flatten()
            .collect();
        for entry_ref in &history.archive_order {
            let Some(TraceObject::HistoryEntry(entry)) =
                known(reduction, &AnyObjectRef::HistoryEntry(entry_ref.clone()))
            else {
                continue;
            };
            let epoch = epochs
                .get(&AnyObjectRef::HistoryEntry(entry_ref.clone()))
                .or_else(|| epochs.get(&AnyObjectRef::Agent(agent.clone())))
                .ok_or_else(|| anyhow::anyhow!("search entry lacks evidence scope"))?;
            let original_owner = match &entry.original_owner {
                Some(OwnerRef::Agent { agent }) => agent.clone(),
                _ => entry.containing_agent.clone(),
            };
            let mut scopes = vec![(SearchHistoryMode::RetainedArchive, None)];
            if original_owner == entry.containing_agent {
                scopes.push((SearchHistoryMode::Authored, None));
            }
            if selected.contains(entry_ref) {
                scopes.push((
                    SearchHistoryMode::SelectedContext,
                    history.selection.known().cloned(),
                ));
            }
            for content in &entry.content {
                let HistoryContent::Message(revision_ref) = content else {
                    continue;
                };
                let Some(TraceObject::MessageRevision(revision)) = known(
                    reduction,
                    &AnyObjectRef::MessageRevision(revision_ref.clone()),
                ) else {
                    continue;
                };
                for part in &revision.parts {
                    if matches!(
                        part.kind,
                        ContentKind::Text
                            | ContentKind::DisclosedReasoning
                            | ContentKind::Structured
                    ) && part.encoding == PayloadEncoding::Utf8
                    {
                        result.push(IndexSource {
                            agent: agent.clone(),
                            original_owner: original_owner.clone(),
                            entry: entry_ref.clone(),
                            revision: revision_ref.clone(),
                            message: revision.message.clone(),
                            representation: revision.representation,
                            part: part.clone(),
                            epoch: epoch.clone(),
                            scopes: scopes.clone(),
                        });
                    }
                }
            }
        }
    }
    Ok(result)
}
