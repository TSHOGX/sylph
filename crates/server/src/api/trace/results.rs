use super::*;
use crate::trace::reading_store::{RESULT_REPRESENTATIONS, result_collection};

pub(super) fn representation(query: &ReadQuery) -> Result<ResultRepresentation> {
    let value = query
        .representation
        .as_ref()
        .ok_or_else(|| invalid("A result representation is required"))?;
    serde_json::from_value(serde_json::Value::String(value.clone()))
        .map_err(|_| invalid("Unknown result representation"))
}

pub(super) async fn authorize_operation(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    reference: &AnyObjectRef,
) -> Result<()> {
    if !matches!(reference, AnyObjectRef::Operation(_)) {
        return Err(invalid("Result inspection requires an operation"));
    }
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
    check_row(state, subject, lease, &row).await
}

struct ProbeResult {
    entry: ReadResultRepresentation,
    witness: Option<ReadCandidate>,
}

struct Catalogue<'a> {
    state: &'a AppState,
    subject: &'a str,
    lease: &'a ViewLease,
    reference: &'a AnyObjectRef,
    selection: Option<&'a SelectionRef>,
    filter: ReadFilter,
}

impl<'a> Catalogue<'a> {
    async fn new(
        state: &'a AppState,
        subject: &'a str,
        lease: &'a ViewLease,
        reference: &'a AnyObjectRef,
        selection: Option<&'a SelectionRef>,
    ) -> Result<Self> {
        Ok(Self {
            state,
            subject,
            lease,
            reference,
            selection,
            filter: ReadFilter {
                allowed_environments: authorized_environments(state, subject).await?,
                ..Default::default()
            },
        })
    }

    async fn validate(&self, entries: &[ProbeResult]) -> Result<()> {
        #[cfg(test)]
        if let Some((entered, release)) = self
            .state
            .trace
            .result_catalogue_read_pause
            .lock()
            .await
            .take()
        {
            entered.notify_one();
            release.notified().await;
        }
        // Presence is optional metadata: check each positive witness again, but
        // do not bind its epoch to the operation's independently retained lease.
        // A deleted witness cannot be replaced with an unproved absence claim.
        for entry in entries {
            if let Some(witness) = &entry.witness {
                if !candidate_is_readable(self.state, self.subject, witness.clone())
                    .await
                    .map_err(storage)?
                {
                    return Err(invalid("Result availability changed; retry inspection"));
                }
            }
        }
        if authorized_environments(self.state, self.subject).await?
            != self.filter.allowed_environments
        {
            return Err(invalid("Result authorization changed; retry inspection"));
        }
        Ok(())
    }

    async fn probe(
        &self,
        representation: ResultRepresentation,
        mut phase: ReadResultProbePhase,
        mut after: Option<&str>,
    ) -> Result<ProbeResult> {
        let generation = self
            .state
            .trace
            .generation(&self.lease.snapshot.generation)
            .await
            .map_err(internal)?;
        let (state, witness) = loop {
            let collection = result_collection(
                self.reference,
                representation,
                phase == ReadResultProbePhase::Payload,
            );
            let scope = read_scope(self.reference.scope(), self.selection, &collection);
            // A presence proof cannot borrow an ordinary result page's cursor:
            // that could skip the only visible result and falsely prove absence.
            let cursor_scope = format!("result-catalogue:{scope}");
            let cursor = open_cursor(
                self.state,
                self.subject,
                self.lease,
                &cursor_scope,
                &self.filter,
                false,
                after,
            )?;
            let rows = generation
                .read_authorized_page(
                    &self.lease.snapshot,
                    &scope,
                    ReadWindow {
                        before: None,
                        after: cursor.as_deref(),
                        around: None,
                        limit: 1,
                        latest: false,
                    },
                    &self.filter,
                    |candidate| candidate_is_readable(self.state, self.subject, candidate),
                )
                .await
                .map_err(storage)?;
            if let Some(row) = rows.rows.first() {
                let witness = ReadCandidate {
                    epoch: row.epoch.clone(),
                    dependency_epochs: row.dependency_epochs.clone(),
                    required_environments: row.required_environments.clone(),
                };
                break (
                    match phase {
                        ReadResultProbePhase::Payload => ReadResultRepresentationState::Available,
                        ReadResultProbePhase::Occurrence => {
                            ReadResultRepresentationState::Unavailable {
                                reason: "Observed result bytes are unavailable in this view".into(),
                            }
                        }
                    },
                    Some(witness),
                );
            }
            if rows.has_after || rows.work_exhausted {
                let cursor = seal_cursor(
                    self.state,
                    self.subject,
                    self.lease,
                    &cursor_scope,
                    &self.filter,
                    false,
                    rows.after,
                )?
                .ok_or_else(|| internal("An unfinished result probe has no continuation"))?;
                break (
                    ReadResultRepresentationState::Pending { phase, cursor },
                    None,
                );
            }
            match phase {
                ReadResultProbePhase::Payload => {
                    phase = ReadResultProbePhase::Occurrence;
                    after = None;
                }
                ReadResultProbePhase::Occurrence => {
                    break (ReadResultRepresentationState::Unobserved, None);
                }
            }
        };
        Ok(ProbeResult {
            entry: ReadResultRepresentation {
                representation,
                state,
            },
            witness,
        })
    }
}

pub(super) async fn catalogue(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    reference: &AnyObjectRef,
    selection: Option<&SelectionRef>,
) -> Result<Vec<ReadResultRepresentation>> {
    let catalogue = Catalogue::new(state, subject, lease, reference, selection).await?;
    let mut entries = Vec::new();
    for representation in RESULT_REPRESENTATIONS {
        entries.push(
            catalogue
                .probe(representation, ReadResultProbePhase::Payload, None)
                .await?,
        );
    }
    catalogue.validate(&entries).await?;
    Ok(entries.into_iter().map(|entry| entry.entry).collect())
}

pub(super) async fn continue_probe(
    state: &AppState,
    subject: &str,
    lease: &ViewLease,
    reference: &AnyObjectRef,
    selection: Option<&SelectionRef>,
    query: &ReadQuery,
) -> Result<ReadResultRepresentation> {
    if query.after.is_some() != query.result_phase.is_some()
        || query.before.is_some()
        || query.around.is_some()
    {
        return Err(invalid(
            "A result probe continuation requires its phase and forward cursor",
        ));
    }
    let catalogue = Catalogue::new(state, subject, lease, reference, selection).await?;
    let entry = catalogue
        .probe(
            representation(query)?,
            query.result_phase.unwrap_or(ReadResultProbePhase::Payload),
            query.after.as_deref(),
        )
        .await?;
    catalogue.validate(std::slice::from_ref(&entry)).await?;
    Ok(entry.entry)
}
