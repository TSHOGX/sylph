//! Optional proof rows never add dependencies to independently retained bodies.
use super::*;
use crate::trace::reading_store::CopyAuthorizationNode;

pub(super) fn materialize(
    rows: &mut BTreeMap<(String, String), ReadRowChange>,
    projection: &ReadingProjection,
    provenance: &ProvenanceMap,
    reduction: &Reduction,
    support_index: &provenance::SupportIndex,
) -> Result<()> {
    let mut insert =
        |scope: String, address: String, entry: &EntryRef, body: serde_json::Value| -> Result<()> {
            let support = object_support(
                reduction,
                provenance,
                &AnyObjectRef::HistoryEntry(entry.clone()),
            )?;
            rows.insert(
                (scope.clone(), address.clone()),
                ReadRowChange {
                    directory: None,
                    scope,
                    address,
                    anchor: Some(TraceAddress::of(&AnyObjectRef::HistoryEntry(entry.clone()))),
                    order_key: vec![0],
                    body: Some(body),
                    scope_epoch: support.primary_epoch.clone(),
                    dependency_epochs: support.dependency_epochs.iter().cloned().collect(),
                    required_environments: BTreeSet::from([entry
                        .scope()
                        .environment()
                        .as_str()
                        .to_owned()]),
                },
            );
            Ok(())
        };
    for (key, node) in &projection.copy_nodes {
        let groups = node
            .groups
            .iter()
            .map(|alternatives| {
                alternatives
                    .iter()
                    .map(|claim| {
                        support_index
                            .claim_dependencies(claim)
                            .map(|epochs| epochs.iter().cloned().collect())
                            .ok_or_else(|| {
                                WorkerError::Task(
                                    "Copy provenance has no retained claim support".into(),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        insert(
            read_scope(key.entry.scope(), None, "copy-proof-nodes"),
            key.address(),
            &key.entry,
            serde_json::to_value(CopyAuthorizationNode {
                next: node.next.clone(),
                groups,
            })?,
        )?;
    }
    for (scope, units) in &projection.windows {
        for unit in units {
            if !matches!(unit.entry_context, Some(ReadEntryContext::Inherited { .. })) {
                continue;
            }
            let AnyObjectRef::HistoryEntry(entry) = unit.anchor.object() else {
                continue;
            };
            let roots = projection
                .copy_contexts
                .get(&(scope.clone(), entry.clone()))
                .map(|proof| proof.roots.as_slice())
                .unwrap_or_default();
            insert(
                read_scope(&scope.agent, scope.selection.as_ref(), "copy-context"),
                entry.encode(),
                &entry,
                serde_json::to_value(roots)?,
            )?;
        }
    }
    Ok(())
}
