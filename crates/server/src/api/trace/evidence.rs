//! On-demand evidence queries authorize retained support before exposing metadata.
use super::*;
use crate::trace::generation::{GenerationStore, Snapshot};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceQuery {
    at_snapshot: String,
    selection: Option<String>,
    corrects: Option<String>,
    after: Option<String>,
    limit: Option<usize>,
    start: Option<u64>,
    end: Option<u64>,
}
fn query(
    value: std::result::Result<Query<EvidenceQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<EvidenceQuery> {
    value
        .map(|Query(value)| value)
        .map_err(|error| invalid(error.to_string()))
}
fn missing() -> TraceError {
    storage(GenerationError::NotFound)
}
fn private_json(value: impl Serialize) -> Response {
    let mut response = Json(value).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store, no-transform"),
    );
    response
}
async fn support(
    state: &AppState,
    subject: &str,
    generation: &GenerationStore,
    snapshot: &Snapshot,
    reference: &AnyObjectRef,
    claim: &ClaimRef,
    maximum: usize,
) -> Result<Option<(Vec<ObservationRef>, BTreeSet<IngestionEpoch>)>> {
    let observations = generation
        .claim_support(snapshot, reference, claim, maximum)
        .await
        .map_err(storage)?;
    let mut epochs = BTreeSet::new();
    for observation in &observations {
        let Some(epoch) = state
            .trace
            .evidence()
            .observation_epoch(observation, snapshot.evidence_cut)
            .await
            .map_err(internal)?
        else {
            return Ok(None);
        };
        epochs.insert(epoch);
    }
    for epoch in &epochs {
        let proof = state
            .trace
            .catalogue()
            .scope_proof(epoch)
            .await
            .map_err(internal)?;
        if !state
            .trace
            .catalogue()
            .scope_is_readable(&proof)
            .await
            .map_err(internal)?
            || !state
                .store
                .has_capability(subject, proof.scope.environment().as_str(), "observe")
                .await
                .map_err(internal)?
        {
            return Ok(None);
        }
    }
    Ok(Some((observations, epochs)))
}
async fn selection_check(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    reference: &AnyObjectRef,
    selected: Option<&str>,
) -> Result<()> {
    let selected = selected
        .map(SelectionRef::decode)
        .transpose()
        .map_err(|error| invalid(error.to_string()))?;
    check_selected_object(state, subject, lease, selected.as_ref(), reference).await
}

pub(super) async fn list(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(encoded): Path<String>,
    input: std::result::Result<Query<EvidenceQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response> {
    let query = query(input)?;
    if query.start.is_some() || query.end.is_some() {
        return Err(invalid("Claim pages do not accept byte ranges"));
    }
    let reference = reference(&encoded)?;
    observe(&state, &subject.0, reference.scope()).await?;
    let lease = lease(&state, &subject.0, Some(&query.at_snapshot)).await?;
    selection_check(
        &state,
        &subject.0,
        &lease,
        &reference,
        query.selection.as_deref(),
    )
    .await?;
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    // The retained subject index remains valid when an interpretation was withdrawn.
    generation
        .summary(&lease.snapshot, &reference)
        .await
        .map_err(storage)?;
    let limit = query.limit.unwrap_or(8).min(state.trace.limits().max_items);
    if limit == 0 {
        return Err(invalid("Claim page limit must be positive"));
    }
    let corrects = query
        .corrects
        .as_deref()
        .map(ClaimRef::decode)
        .transpose()
        .map_err(|error| invalid(error.to_string()))?;
    let scope = serde_json::to_string(&("evidence", &reference, &query.selection, &corrects))
        .map_err(internal)?;
    let filter = ReadFilter {
        allowed_environments: authorized_environments(&state, &subject.0).await?,
        ..Default::default()
    };
    let mut cursor = open_cursor(
        &state,
        &subject.0,
        &lease,
        &scope,
        &filter,
        false,
        query.after.as_deref(),
    )?;
    let mut items = vec![];
    let mut bytes = 2;
    let mut work = 0;
    let mut returned_epochs = BTreeSet::new();
    let mut has_more = true;
    while items.len() < limit && work < generation.limits().max_candidates && has_more {
        let page = if let Some(prior) = &corrects {
            generation
                .claim_corrections(&lease.snapshot, &reference, prior, cursor.as_deref(), 1)
                .await
        } else {
            generation
                .claims_for(&lease.snapshot, &reference, None, cursor.as_deref(), 1)
                .await
        }
        .map_err(storage)?;
        let Some(candidate) = page.claims.into_iter().next() else {
            has_more = false;
            break;
        };
        let cost = usize::try_from(candidate.evidence_count)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        if work.saturating_add(cost) > generation.limits().max_candidates {
            if work == 0 {
                return Err(storage(GenerationError::Limit("claim support work")));
            }
            work = generation.limits().max_candidates;
            break;
        }
        work += cost;
        let supported = support(
            &state,
            &subject.0,
            &generation,
            &lease.snapshot,
            &reference,
            &candidate.claim_ref,
            generation.limits().max_candidates,
        )
        .await?;
        if let Some((_, epochs)) = supported {
            let item = ReadClaimSummary {
                preview: candidate.preview,
                preview_truncated: candidate.preview_truncated,
                claim_ref: candidate.claim_ref,
                fact_kind: candidate.fact_kind,
                mapper_build: candidate.mapper_build,
                rule_id: candidate.rule_id,
                evidence_count: candidate.evidence_count,
                detail_bytes: candidate.detail_bytes,
            };
            let size = serde_json::to_vec(&item).map_err(internal)?.len() + 1;
            if bytes + size > state.trace.limits().max_summary_bytes {
                if items.is_empty() {
                    return Err(storage(GenerationError::Limit("claim summary bytes")));
                }
                break;
            }
            check_epochs(&state, &subject.0, &lease, &epochs).await?;
            returned_epochs.extend(epochs);
            bytes += size;
            items.push(item);
        }
        cursor = page.next_cursor;
        has_more = page.has_more;
    }
    check_epochs(&state, &subject.0, &lease, &returned_epochs).await?;
    let response = ReadPage {
        view: read_view(&lease),
        returned_items: items.len(),
        returned_bytes: serde_json::to_vec(&items).map_err(internal)?.len(),
        items,
        before_cursor: None,
        after_cursor: seal_cursor(
            &state,
            &subject.0,
            &lease,
            &scope,
            &filter,
            false,
            if has_more { cursor } else { None },
        )?,
        has_before: false,
        has_after: has_more,
        total: ReadCount {
            value: None,
            scope: "authorized retained claims".into(),
            complete: false,
        },
        state: if has_more {
            if work >= generation.limits().max_candidates {
                ReadPageState::WorkBudgetExhausted
            } else {
                ReadPageState::More
            }
        } else {
            ReadPageState::Complete
        },
        limits: state.trace.limits().clone(),
    };
    lease.retain();
    Ok(private_json(response))
}

pub(super) async fn detail(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path((encoded, claim)): Path<(String, String)>,
    input: std::result::Result<Query<EvidenceQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response> {
    let query = query(input)?;
    if query.corrects.is_some()
        || query.after.is_some()
        || query.limit.is_some()
        || query.start.is_some()
        || query.end.is_some()
    {
        return Err(invalid(
            "Claim detail does not accept pagination or byte ranges",
        ));
    }
    let reference = reference(&encoded)?;
    let claim = ClaimRef::decode(&claim).map_err(|error| invalid(error.to_string()))?;
    observe(&state, &subject.0, reference.scope()).await?;
    let lease = lease(&state, &subject.0, Some(&query.at_snapshot)).await?;
    selection_check(
        &state,
        &subject.0,
        &lease,
        &reference,
        query.selection.as_deref(),
    )
    .await?;
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let (observations, epochs) = support(
        &state,
        &subject.0,
        &generation,
        &lease.snapshot,
        &reference,
        &claim,
        generation.limits().max_candidates,
    )
    .await?
    .ok_or_else(missing)?;
    check_epochs(&state, &subject.0, &lease, &epochs).await?;
    let claim = generation
        .claim(&lease.snapshot, &claim)
        .await
        .map_err(storage)?;
    let mut related = claim.fact.subject_refs();
    if let Fact::Declare {
        object: TraceObject::MessageRevision(revision),
    } = &claim.fact
    {
        related.push(AnyObjectRef::Message(revision.message.clone()));
    }
    related.retain(|value| value != &reference);
    let related = related
        .into_iter()
        .collect::<BTreeSet<_>>()
        .iter()
        .map(TraceAddress::of)
        .collect();
    check_epochs(&state, &subject.0, &lease, &epochs).await?;
    let detail = ReadClaimDetail {
        view: read_view(&lease),
        subject: TraceAddress::of(&reference),
        sources: observations.iter().map(ObservationRef::encode).collect(),
        claim,
        related,
    };
    if serde_json::to_vec(&detail).map_err(internal)?.len() > generation.limits().max_object_bytes {
        return Err(storage(GenerationError::Limit("claim detail bytes")));
    }
    lease.retain();
    Ok(private_json(detail))
}

pub(super) async fn source(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path((encoded, claim, observation)): Path<(String, String, String)>,
    input: std::result::Result<Query<EvidenceQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response> {
    let query = query(input)?;
    if query.corrects.is_some() || query.after.is_some() || query.limit.is_some() {
        return Err(invalid("Source reads accept byte ranges, not page cursors"));
    }
    let reference = reference(&encoded)?;
    let claim = ClaimRef::decode(&claim).map_err(|error| invalid(error.to_string()))?;
    let observation =
        ObservationRef::decode(&observation).map_err(|error| invalid(error.to_string()))?;
    observe(&state, &subject.0, reference.scope()).await?;
    let lease = lease(&state, &subject.0, Some(&query.at_snapshot)).await?;
    selection_check(
        &state,
        &subject.0,
        &lease,
        &reference,
        query.selection.as_deref(),
    )
    .await?;
    let generation = state
        .trace
        .generation(&lease.snapshot.generation)
        .await
        .map_err(internal)?;
    let (observations, epochs) = support(
        &state,
        &subject.0,
        &generation,
        &lease.snapshot,
        &reference,
        &claim,
        generation.limits().max_candidates,
    )
    .await?
    .ok_or_else(missing)?;
    if !observations.contains(&observation) {
        return Err(missing());
    }
    check_epochs(&state, &subject.0, &lease, &epochs).await?;
    let capture = state
        .trace
        .evidence()
        .retained_capture(
            &observation,
            lease.snapshot.evidence_cut,
            generation.limits().max_object_bytes,
        )
        .await
        .map_err(internal)?
        .ok_or_else(missing)?;
    let start = query.start.unwrap_or(0);
    let maximum = state.trace.limits().max_payload_bytes.min(64 * 1024) as u64;
    let total = state
        .trace
        .evidence()
        .payload_length(&capture.payload_ref)
        .await
        .map_err(internal)?
        .ok_or_else(|| storage(GenerationError::Limit("unknown source length")))?;
    let end = query
        .end
        .unwrap_or(start.saturating_add(maximum).min(total));
    if end < start || end > total || end - start > maximum || (end == start && total != 0) {
        return Err(invalid(
            "Source range is outside the retained length or byte limit",
        ));
    }
    let range = if total == 0 {
        crate::trace::evidence::PayloadSlice {
            bytes: vec![],
            offset: 0,
            total_length: Some(0),
            coverage: PayloadCompleteness::Complete,
        }
    } else {
        state
            .trace
            .evidence()
            .payload_range(&capture.payload_ref, start, (end - start) as usize)
            .await
            .map_err(internal)?
    };
    #[cfg(test)]
    if let Some((entered, release)) = state.trace.evidence_read_pause.lock().await.take() {
        entered.notify_one();
        release.notified().await;
    }
    check_epochs(&state, &subject.0, &lease, &epochs).await?;
    let response = ReadEvidenceBytes {
        max_range_bytes: maximum as usize,
        view: read_view(&lease),
        capture,
        start: range.offset,
        end_exclusive: range.offset + range.bytes.len() as u64,
        total_bytes: range.total_length,
        coverage: range.coverage,
        bytes_base64: STANDARD.encode(range.bytes),
    };
    lease.retain();
    Ok(private_json(response))
}
