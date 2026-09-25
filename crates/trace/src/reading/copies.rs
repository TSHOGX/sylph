//! Copy provenance follows exact version nodes. Shared tails are resolved once;
//! authorization keeps immediate proof groups instead of materializing every path.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CopyNodeKey {
    pub entry: EntryRef,
    pub position: SourcePosition,
}
impl CopyNodeKey {
    pub fn address(&self) -> String {
        blake3::hash(&serde_json::to_vec(self).expect("typed copy node"))
            .to_hex()
            .to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyProofNode {
    pub next: Option<CopyNodeKey>,
    /// Every group needs one independently authorized retained claim. Agreeing
    /// captures remain alternatives; deleting one does not erase another proof.
    pub groups: Vec<Vec<ClaimRef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyContextProof {
    pub roots: Vec<CopyNodeKey>,
    pub context: ReadEntryContext,
}

#[derive(Default)]
pub(super) struct CopyIndex {
    pub(super) nodes: BTreeMap<CopyNodeKey, CopyProofNode>,
    resolutions: BTreeMap<CopyNodeKey, Option<Option<AgentRef>>>,
    copied: BTreeSet<EntryRef>,
    edges: BTreeSet<CopyNodeKey>,
}

impl CopyIndex {
    pub(super) fn new(reduction: &Reduction) -> Self {
        let mut index = Self::default();
        let mut edges = BTreeMap::<CopyNodeKey, BTreeMap<CopyNodeKey, Vec<ClaimRef>>>::new();
        for relation in &reduction.relations {
            if let Relation::ContextCopy {
                original,
                copied,
                original_position,
                copied_position,
            } = &relation.relation
            {
                index.copied.insert(copied.clone());
                edges
                    .entry(CopyNodeKey {
                        entry: copied.clone(),
                        position: copied_position.clone(),
                    })
                    .or_default()
                    .entry(CopyNodeKey {
                        entry: original.clone(),
                        position: original_position.clone(),
                    })
                    .or_default()
                    .push(relation.claim.clone());
            }
        }
        if edges.is_empty() {
            return index;
        }
        index.edges.extend(edges.keys().cloned());
        let relevant: BTreeSet<_> = edges
            .keys()
            .chain(edges.values().flat_map(|edges| edges.keys()))
            .cloned()
            .collect();
        let mut observations = BTreeMap::<CopyNodeKey, Vec<ClaimRef>>::new();
        for (entry, contents) in &reduction.history_contents {
            for (claim, observation) in contents {
                let node = CopyNodeKey {
                    entry: entry.clone(),
                    position: observation.position.clone(),
                };
                if relevant.contains(&node) && observation.complete {
                    observations.entry(node).or_default().push(claim.clone());
                }
            }
        }
        let mut terminals = BTreeMap::new();
        for node in &relevant {
            let Some(contents) = observations.get(node) else {
                continue;
            };
            let Some(object) = reduction.object(&AnyObjectRef::HistoryEntry(node.entry.clone()))
            else {
                continue;
            };
            let Resolved::Known {
                value: TraceObject::HistoryEntry(entry),
                claims,
            } = &object.declaration
            else {
                continue;
            };
            if let Some(origins) = edges.get(node) {
                if origins.len() != 1 {
                    continue;
                }
                let (next, relation_claims) = origins.iter().next().unwrap();
                // A resolved declaration can combine complementary unknown-to-known
                // fields, so its claims are conjunctive. Repeated copy witnesses
                // and complete observations of one version are alternatives.
                index.nodes.insert(
                    node.clone(),
                    CopyProofNode {
                        next: Some(next.clone()),
                        groups: std::iter::once(contents.clone())
                            .chain(claims.iter().map(|claim| vec![claim.clone()]))
                            .chain(std::iter::once(relation_claims.clone()))
                            .collect(),
                    },
                );
            } else {
                // No edge at this exact version is not proof of local origin when
                // another version has a copy witness. Never cross that version gap.
                let owner = match &entry.original_owner {
                    Some(OwnerRef::Agent { agent }) => Some(agent.clone()),
                    None if !index.copied.contains(&node.entry) => {
                        Some(entry.containing_agent.clone())
                    }
                    _ => None,
                };
                terminals.insert(node.clone(), owner);
                index.nodes.insert(
                    node.clone(),
                    CopyProofNode {
                        next: None,
                        groups: std::iter::once(contents.clone())
                            .chain(claims.iter().map(|claim| vec![claim.clone()]))
                            .collect(),
                    },
                );
            }
        }
        // Iterative memoization bounds deep chains and cycles without recursion
        // or repeatedly walking every ancestor for every copied entry.
        for root in relevant {
            if index.resolutions.contains_key(&root) {
                continue;
            }
            let mut current = root;
            let mut path = Vec::new();
            let mut visiting = BTreeSet::new();
            let resolved = loop {
                if let Some(known) = index.resolutions.get(&current) {
                    break known.clone();
                }
                if !visiting.insert(current.clone()) {
                    break None;
                }
                path.push(current.clone());
                let Some(node) = index.nodes.get(&current) else {
                    break None;
                };
                match &node.next {
                    Some(next) => current = next.clone(),
                    None => break Some(terminals[&current].clone()),
                }
            };
            for node in path {
                index.resolutions.insert(node, resolved.clone());
            }
        }
        index
    }

    pub(super) fn context(
        &self,
        reduction: &Reduction,
        entry: &EntryRef,
        selection: Option<&SelectionRef>,
    ) -> Option<CopyContextProof> {
        if !self.copied.contains(entry) {
            return None;
        }
        if matches!(
            reduction
                .object(&AnyObjectRef::HistoryEntry(entry.clone()))
                .and_then(|object| object.declaration.known()),
            Some(TraceObject::HistoryEntry(HistoryEntry {
                original_owner: Some(OwnerRef::Agent { .. }),
                ..
            }))
        ) {
            return None;
        }
        let unresolved = || CopyContextProof {
            roots: vec![],
            context: ReadEntryContext::Unresolved,
        };
        let allowed = match selection {
            None => None,
            Some(selection) => match reduction.selection_content_observations.get(selection) {
                Some(Some(allowed)) => Some(allowed),
                _ => return Some(unresolved()),
            },
        };
        let roots: BTreeSet<_> = reduction
            .history_contents
            .get(entry)
            .into_iter()
            .flatten()
            .filter(|(claim, _)| allowed.is_none_or(|allowed| allowed.contains(*claim)))
            .map(|(_, observation)| CopyNodeKey {
                entry: entry.clone(),
                position: observation.position.clone(),
            })
            .collect();
        if roots.is_empty() {
            return Some(unresolved());
        }
        let mut owner = None;
        for node in &roots {
            let Some(Some(resolved)) = self
                .resolutions
                .get(node)
                .filter(|_| self.edges.contains(node))
            else {
                return Some(unresolved());
            };
            if owner.as_ref().is_some_and(|owner| owner != resolved) {
                return Some(unresolved());
            }
            owner = Some(resolved.clone());
        }
        let original_owner = owner
            .flatten()
            .map(|agent| super::agent_link_from_reduction(reduction, &agent));
        Some(CopyContextProof {
            roots: roots.into_iter().collect(),
            context: ReadEntryContext::Inherited { original_owner },
        })
    }
}
