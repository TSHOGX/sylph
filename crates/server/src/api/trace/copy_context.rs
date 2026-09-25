//! Bounded optional lineage checks do not enroll remote epochs in a body lease.
use super::*;
use crate::trace::reading_store::CopyAuthorizationNode;
use std::collections::{BTreeMap, BTreeSet};
use sylph_trace::reading::CopyNodeKey;

#[derive(Default)]
pub(super) struct Authorization {
    nodes: BTreeMap<CopyNodeKey, bool>,
    epochs: BTreeMap<IngestionEpoch, bool>,
    work: usize,
    visibility: Option<(u64, Option<BTreeSet<String>>)>,
}

impl Authorization {
    fn charge(&mut self, limit: usize) -> bool {
        if self.work >= limit {
            return false;
        }
        self.work += 1;
        true
    }

    async fn node(
        &mut self,
        state: &AppState,
        subject: &str,
        generation: &crate::trace::generation::GenerationStore,
        snapshot: &crate::trace::generation::Snapshot,
        root: CopyNodeKey,
    ) -> Result<bool> {
        let limit = generation.limits().max_candidates;
        let mut current = root;
        let mut path = BTreeSet::new();
        let readable = 'chain: loop {
            if let Some(known) = self.nodes.get(&current) {
                break *known;
            }
            if !path.insert(current.clone()) || !self.charge(limit) {
                break false;
            }
            let row = match generation
                .read_row(
                    snapshot,
                    &read_scope(current.entry.scope(), None, "copy-proof-nodes"),
                    &current.address(),
                )
                .await
            {
                Ok(row) => row,
                Err(GenerationError::NotFound) => break false,
                Err(error) => return Err(storage(error)),
            };
            let node: CopyAuthorizationNode = serde_json::from_value(row.body).map_err(internal)?;
            if node.groups.is_empty() {
                break false;
            }
            for alternatives in node.groups {
                let mut authorized = false;
                for proof in alternatives {
                    if !self.charge(limit) {
                        break 'chain false;
                    }
                    if proof.is_empty() {
                        continue;
                    }
                    let mut alive = true;
                    for epoch in proof {
                        if !self.charge(limit) {
                            break 'chain false;
                        }
                        let readable = match self.epochs.get(&epoch) {
                            Some(readable) => *readable,
                            None => {
                                let readable = candidate_is_readable(
                                    state,
                                    subject,
                                    ReadCandidate {
                                        epoch: epoch.clone(),
                                        dependency_epochs: BTreeSet::new(),
                                        required_environments: BTreeSet::new(),
                                    },
                                )
                                .await
                                .map_err(storage)?;
                                self.epochs.insert(epoch, readable);
                                readable
                            }
                        };
                        if !readable {
                            alive = false;
                            break;
                        }
                    }
                    if alive {
                        authorized = true;
                        break;
                    }
                }
                if !authorized {
                    break 'chain false;
                }
            }
            match node.next {
                Some(next) => current = next,
                None => break true,
            }
        };
        for node in path {
            self.nodes.insert(node, readable);
        }
        Ok(readable)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn redact(
        &mut self,
        state: &AppState,
        subject: &str,
        generation: &crate::trace::generation::GenerationStore,
        snapshot: &crate::trace::generation::Snapshot,
        selection: Option<&SelectionRef>,
        value: &mut serde_json::Value,
    ) -> Result<()> {
        if value
            .pointer("/entryContext/kind")
            .and_then(serde_json::Value::as_str)
            != Some("inherited")
        {
            return Ok(());
        }
        if self.visibility.is_none() {
            self.visibility = Some((
                state
                    .trace
                    .catalogue()
                    .visibility_revision()
                    .await
                    .map_err(internal)?,
                authorized_environments(state, subject).await?,
            ));
        }
        let entry = value
            .get("anchor")
            .and_then(serde_json::Value::as_str)
            .and_then(|anchor| TraceAddress::parse(anchor).ok())
            .map(|anchor| anchor.object());
        let readable = if let Some(AnyObjectRef::HistoryEntry(entry)) = entry {
            if !self.charge(generation.limits().max_candidates) {
                false
            } else {
                let roots = match generation
                    .read_row(
                        snapshot,
                        &read_scope(entry.scope(), selection, "copy-context"),
                        &entry.encode(),
                    )
                    .await
                {
                    Ok(row) => Some(
                        serde_json::from_value::<Vec<CopyNodeKey>>(row.body).map_err(internal)?,
                    ),
                    Err(GenerationError::NotFound) => None,
                    Err(error) => return Err(storage(error)),
                };
                if let Some(roots) = roots {
                    let mut readable = true;
                    for root in roots {
                        if !self
                            .node(state, subject, generation, snapshot, root)
                            .await?
                        {
                            readable = false;
                            break;
                        }
                    }
                    readable
                } else {
                    false
                }
            }
        } else {
            false
        };
        if !readable {
            value["entryContext"] = serde_json::json!({"kind":"unresolved"});
        }
        Ok(())
    }

    pub(super) async fn finish(
        &self,
        state: &AppState,
        subject: &str,
        values: &mut [serde_json::Value],
    ) -> Result<()> {
        let Some((revision, grants)) = &self.visibility else {
            return Ok(());
        };
        #[cfg(test)]
        if let Some((entered, release)) = state.trace.copy_context_read_pause.lock().await.take() {
            entered.notify_one();
            release.notified().await;
        }
        if state
            .trace
            .catalogue()
            .visibility_revision()
            .await
            .map_err(internal)?
            != *revision
            || authorized_environments(state, subject).await? != *grants
        {
            for value in values {
                if value
                    .pointer("/entryContext/kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("inherited")
                {
                    value["entryContext"] = serde_json::json!({"kind":"unresolved"});
                }
            }
        }
        Ok(())
    }
}
