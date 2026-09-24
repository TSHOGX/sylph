//! Exact payload authority shared by windows, transfers and structured exploration.
use super::*;
use crate::trace::{generation::PayloadAnchor, search::SearchError};
use std::collections::BTreeSet;

pub(super) struct AuthorizedPayload {
    pub row: ReadRow,
    pub anchor: PayloadAnchor,
    pub ranges: Vec<PayloadRange>,
    pub dependencies: BTreeSet<IngestionEpoch>,
    search_expires_at_ms: Option<i64>,
}

impl AuthorizedPayload {
    pub fn range(&self, start: u64, end: u64) -> Result<()> {
        if start > end {
            return Err(invalid("Payload range is reversed"));
        }
        if start == end {
            return Ok(());
        }
        let mut cursor = start;
        for range in &self.ranges {
            if range.end_exclusive <= cursor {
                continue;
            }
            if range.start > cursor {
                break;
            }
            cursor = range.end_exclusive;
            if cursor >= end {
                return Ok(());
            }
        }
        Err(storage(GenerationError::PayloadNotReachable))
    }

    pub async fn check(&self, state: &AppState, subject: &str, lease: &ViewLease) -> Result<()> {
        if self
            .search_expires_at_ms
            .is_some_and(|expires| crate::models::now_ms() >= expires)
        {
            return Err(search_error(SearchError::Expired));
        }
        check_row(state, subject, lease, &self.row).await?;
        check_epochs(state, subject, lease, &self.dependencies).await
    }
}

fn search_error(error: SearchError) -> TraceError {
    match error {
        SearchError::Expired => TraceError {
            status: StatusCode::GONE,
            code: "snapshot_expired",
            message: "This search result expired; run the search again".into(),
            response: None,
        },
        SearchError::SubjectMismatch => TraceError {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: "This search result belongs to another reader".into(),
            response: None,
        },
        SearchError::InvalidCursor => invalid("Invalid search result reference"),
        other => internal(other),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn authorize(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    via: &str,
    selection: Option<SelectionRef>,
    payload: PayloadRef,
    revision: String,
    representation: Representation,
    search_result: Option<&str>,
) -> Result<AuthorizedPayload> {
    let reference = reference(via)?;
    observe(state, subject, reference.scope()).await?;
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let row = generation
        .read_row(
            &lease.snapshot,
            &read_scope(reference.scope(), None, "objects"),
            &reference.encode(),
        )
        .await
        .map_err(storage)?;
    check_row(state, subject, lease, &row).await?;
    let detail = generation
        .object(&lease.snapshot, &reference)
        .await
        .map_err(storage)?;
    let owner = crate::trace::generation::payload_anchor_owner(&detail.object)
        .cloned()
        .ok_or_else(|| invalid("Payload authorization needs a readable containing object"))?;
    observe(state, subject, &owner).await?;
    let anchor = PayloadAnchor {
        via_ref: reference,
        containing_agent: owner,
        selection,
        payload_ref: payload,
        body_revision: revision,
        representation,
        ranges: vec![],
        dependency_epochs: Default::default(),
    };
    let (ranges, dependencies, search_expires_at_ms) = if let Some(result_ref) = search_result {
        let index = state
            .trace
            .search_store(&lease.snapshot.generation)
            .await
            .map_err(internal)?;
        let result = index
            .read_result(subject, result_ref, crate::models::now_ms())
            .await
            .map_err(search_error)?;
        let body = result.hit.body;
        // This is an explicit alternative authority, never a fallback after a
        // projection miss. Every requested dimension belongs to this one result.
        if result.view != lease.snapshot
            || body.anchor != anchor.via_ref
            || body.agent != anchor.containing_agent
            || body.payload != anchor.payload_ref
            || body.body_revision != anchor.body_revision
            || body.representation != anchor.representation
            || result.hit.context.selection != anchor.selection
        {
            return Err(invalid(
                "Payload request does not match its retained search result",
            ));
        }
        if let Some(selection) = &anchor.selection {
            check_search_selection(state, subject, &generation, lease, selection).await?;
        }
        let dependencies = body
            .dependency_epochs
            .into_iter()
            .chain([body.ingestion_epoch])
            .collect();
        (
            vec![PayloadRange {
                start: 0,
                end_exclusive: body.total_bytes,
            }],
            dependencies,
            Some(result.expires_at_ms),
        )
    } else {
        let (ranges, dependencies) = generation
            .payload_ranges(&lease.snapshot, &anchor)
            .await
            .map_err(storage)?;
        (ranges, dependencies, None)
    };
    let grant = AuthorizedPayload {
        row,
        anchor,
        ranges,
        dependencies,
        search_expires_at_ms,
    };
    grant.check(state, subject, lease).await?;
    Ok(grant)
}

pub(super) async fn context_anchor(
    state: &AppState,
    subject: &str,
    generation: &crate::trace::generation::GenerationStore,
    lease: &ViewLease,
    body: &crate::trace::search::SearchBody,
    selection: Option<&SelectionRef>,
) -> Result<Option<TraceAddress>> {
    let scope = read_scope(&body.agent, selection, "history");
    let original = TraceAddress::of(&body.anchor);
    let candidate = match generation
        .read_candidate(&lease.snapshot, &scope, &original)
        .await
    {
        Ok(candidate) => Some((original, candidate)),
        Err(GenerationError::NotFound) => match &body.object {
            AnyObjectRef::Message(message) => generation
                .message_context(&lease.snapshot, &scope, message)
                .await
                .map_err(storage)?,
            _ => None,
        },
        Err(error) => return Err(storage(error)),
    };
    let Some((anchor, candidate)) = candidate else {
        return Ok(None);
    };
    // Navigation metadata does not lend its retention to the matched body. A
    // destination read rechecks and binds its own proof when the user opens it.
    for environment in &candidate.required_environments {
        if !state
            .store
            .has_capability(subject, environment, "observe")
            .await
            .map_err(internal)?
        {
            return Ok(None);
        }
    }
    Ok(candidate_is_readable(state, subject, candidate)
        .await
        .map_err(storage)?
        .then_some(anchor))
}
