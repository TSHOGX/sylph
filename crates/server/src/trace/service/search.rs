use super::*;
use crate::trace::evidence::catalogue::{PayloadPin, PinOwnerKind};
use crate::trace::search::{SearchDocument, SearchHistoryMode, SearchStore};
use std::collections::BTreeSet;
use sylph_trace::reducer::{Reduction, Resolved};

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
        let mut variants = BTreeMap::<
            (EntryRef, MessageRevisionRef),
            Vec<(SearchHistoryMode, Option<SelectionRef>)>,
        >::new();
        for entry_ref in &history.archive_order {
            let Some(TraceObject::HistoryEntry(entry)) =
                known(reduction, &AnyObjectRef::HistoryEntry(entry_ref.clone()))
            else {
                continue;
            };
            let original_owner = match &entry.original_owner {
                Some(OwnerRef::Agent { agent }) => agent.clone(),
                _ => entry.containing_agent.clone(),
            };
            let mut scopes = vec![(SearchHistoryMode::RetainedArchive, None)];
            if original_owner == entry.containing_agent {
                scopes.push((SearchHistoryMode::Authored, None));
            }
            for revision in bound_revisions(reduction, entry_ref, None) {
                variants.insert((entry_ref.clone(), revision), scopes.clone());
            }
        }
        // Entry membership cannot authorize every body observed for that identity.
        // Resolve the selected bindings independently; archive variants stay archived.
        if let Some((selection, entries)) = history
            .selection
            .known()
            .zip(history.selected_entries.as_ref())
        {
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
                        .push((SearchHistoryMode::SelectedContext, Some(selection.clone())));
                }
            }
        }
        for ((entry_ref, revision_ref), scopes) in variants {
            let Some(TraceObject::HistoryEntry(entry)) =
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
            let epoch = epochs
                .get(&AnyObjectRef::HistoryEntry(entry_ref.clone()))
                .or_else(|| epochs.get(&AnyObjectRef::Agent(agent.clone())))
                .ok_or_else(|| anyhow::anyhow!("search entry lacks evidence scope"))?;
            let original_owner = match &entry.original_owner {
                Some(OwnerRef::Agent { agent }) => agent.clone(),
                _ => entry.containing_agent.clone(),
            };
            for part in &revision.parts {
                if matches!(
                    part.kind,
                    ContentKind::Text | ContentKind::DisclosedReasoning | ContentKind::Structured
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
    Ok(result)
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
            let sources = sources(
                &reducer.resolve(),
                &BTreeMap::from([(
                    AnyObjectRef::Agent(agent()),
                    IngestionEpoch::new("epoch").unwrap(),
                )]),
            )
            .unwrap();
            let archived: BTreeSet<_> = sources
                .iter()
                .filter(|source| {
                    source
                        .scopes
                        .iter()
                        .any(|(mode, _)| mode == &SearchHistoryMode::RetainedArchive)
                })
                .map(|source| source.revision.clone())
                .collect();
            assert_eq!(archived, BTreeSet::from(revisions.clone()));
            let selected: Vec<_> = sources
                .iter()
                .filter(|source| {
                    source
                        .scopes
                        .contains(&(SearchHistoryMode::SelectedContext, Some(selection.clone())))
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
