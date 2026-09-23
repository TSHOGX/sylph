//! Directory continuations certify a scanned prefix under current authority inputs.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

async fn readable(
    state: &AppState,
    subject: &str,
    candidate: ReadCandidate,
    known: &Mutex<BTreeMap<IngestionEpoch, bool>>,
) -> std::result::Result<bool, GenerationError> {
    for epoch in std::iter::once(candidate.epoch)
        .chain(candidate.dependency_epochs)
        .collect::<BTreeSet<_>>()
    {
        let cached = known.lock().unwrap().get(&epoch).copied();
        let allowed = if let Some(cached) = cached {
            cached
        } else {
            let allowed = candidate_is_readable(
                state,
                subject,
                ReadCandidate {
                    epoch: epoch.clone(),
                    dependency_epochs: BTreeSet::new(),
                    required_environments: BTreeSet::new(),
                },
            )
            .await?;
            known.lock().unwrap().insert(epoch, allowed);
            allowed
        };
        if !allowed {
            return Ok(false);
        }
    }
    Ok(true)
}
fn filter(query: &ReadQuery, allowed_environments: Option<BTreeSet<String>>) -> Result<ReadFilter> {
    if query.q.as_ref().is_some_and(|query| query.len() > 512)
        || query
            .relation
            .as_deref()
            .is_some_and(|kind| kind != "spawn")
        || query
            .direction
            .as_deref()
            .is_some_and(|direction| direction != "outgoing")
        || query.before.is_some()
        || query.around.is_some()
        || query.selection.is_some()
        || query.kind.is_some()
    {
        return Err(invalid("Invalid spawn directory query"));
    }
    let state = query.filter.as_deref().unwrap_or("all");
    if !matches!(
        state,
        "all" | "active" | "needs_input" | "completed" | "unknown" | "failed" | "cancelled"
    ) {
        return Err(invalid("Unknown directory filter"));
    }
    Ok(ReadFilter {
        text: query.q.as_deref().unwrap_or("").trim().to_lowercase(),
        state: if state == "all" {
            String::new()
        } else {
            state.into()
        },
        allowed_environments,
    })
}
async fn verify_inputs(
    state: &AppState,
    subject: &str,
    revision: u64,
    filter: &ReadFilter,
) -> Result<()> {
    if state
        .trace
        .catalogue()
        .visibility_revision()
        .await
        .map_err(internal)?
        != revision
        || authorized_environments(state, subject).await? != filter.allowed_environments
    {
        return Err(invalid(
            "Directory visibility changed; restart from the first page",
        ));
    }
    Ok(())
}
pub(super) async fn descendants(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    root: &AgentRef,
) -> Result<Option<u64>> {
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let revision = state
        .trace
        .catalogue()
        .visibility_revision()
        .await
        .map_err(internal)?;
    let filter = ReadFilter {
        allowed_environments: authorized_environments(state, subject).await?,
        ..Default::default()
    };
    let known = Mutex::new(BTreeMap::new());
    let page = generation
        .directory_page(&lease.snapshot, root, None, 0, &filter, |candidate| {
            readable(state, subject, candidate, &known)
        })
        .await
        .map_err(storage)?;
    verify_inputs(state, subject, revision, &filter).await?;
    Ok(page.total.map(|count| count.saturating_sub(1)))
}
pub(super) async fn page(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    root: &AgentRef,
    query: &ReadQuery,
) -> Result<ReadPage<AgentDirectoryRow>> {
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let revision = state
        .trace
        .catalogue()
        .visibility_revision()
        .await
        .map_err(internal)?;
    let filter = filter(query, authorized_environments(state, subject).await?)?;
    let count_only = query.count_only.unwrap_or(false);
    let scope = serde_json::to_string(&("spawn-directory", root, revision, count_only))
        .map_err(internal)?;
    let cursor = open_cursor(
        state,
        subject,
        lease,
        &scope,
        &filter,
        false,
        query.after.as_deref(),
    )?;
    let limit = if count_only {
        0
    } else {
        query
            .limit
            .unwrap_or(40)
            .min(state.trace.limits().max_items)
    };
    if !count_only && limit == 0 {
        return Err(invalid("Directory page limit must be positive"));
    }
    let known = Mutex::new(BTreeMap::new());
    let page = generation
        .directory_page(
            &lease.snapshot,
            root,
            cursor.as_deref(),
            limit,
            &filter,
            |candidate| readable(state, subject, candidate, &known),
        )
        .await
        .map_err(storage)?;
    #[cfg(test)]
    if let Some((entered, release)) = state.trace.directory_read_pause.lock().await.take() {
        entered.notify_one();
        release.notified().await;
    }
    let mut items = vec![];
    let mut current = lease.clone();
    let mut owned = None;
    let mut displayed_epochs = BTreeSet::new();
    for row in page.rows {
        displayed_epochs.insert(row.epoch.clone());
        displayed_epochs.extend(row.dependency_epochs.iter().cloned());
        match check_row(state, subject, &current, &row).await {
            Ok(()) => {}
            Err(error) if error.code == "read_capacity" && owned.is_none() => {
                let fresh = state
                    .trace
                    .window_lease(subject, &lease.token)
                    .await
                    .map_err(lease_error)?;
                check_epochs(state, subject, &fresh, &displayed_epochs).await?;
                current = (*fresh).clone();
                owned = Some(fresh);
            }
            Err(error) => return Err(error),
        }
        let mut value: AgentDirectoryRow = serde_json::from_value(
            authorized_value(state, subject, &generation, &lease.snapshot, row.body).await?,
        )
        .map_err(internal)?;
        if value.agent.address.agent() == *root {
            value.relation = "focus".into();
        } else {
            value.relation = "spawn".into();
        }
        items.push(value);
    }
    verify_inputs(state, subject, revision, &filter).await?;
    let result = ReadPage {
        view: read_view(&current),
        returned_items: items.len(),
        returned_bytes: serde_json::to_vec(&items).map_err(internal)?.len(),
        items,
        before_cursor: None,
        has_before: false,
        after_cursor: seal_cursor(state, subject, &current, &scope, &filter, false, page.after)?,
        has_after: page.has_more,
        total: ReadCount {
            value: page.total,
            scope: "authorized focused agent and spawn descendants".into(),
            complete: page.total.is_some(),
        },
        state: if page.work_exhausted {
            ReadPageState::WorkBudgetExhausted
        } else if page.has_more {
            ReadPageState::More
        } else {
            ReadPageState::Complete
        },
        limits: state.trace.limits().clone(),
    };
    if let Some(owned) = owned {
        owned.retain();
    }
    Ok(result)
}
