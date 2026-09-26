//! RPC writes establish request correlation, never vendor content or admission.
//! One verified baseline and bounded outstanding requests replace history-sized
//! checkpoint sets. The immutable selection chain retains the actual proof.
use super::*;

pub(super) const MAX_PENDING: usize = 32;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct State {
    baseline: Option<Baseline>,
    pending: BTreeMap<String, Request>,
    last_position: Option<SourcePosition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Baseline {
    selection: SelectionRef,
    tail: Option<String>,
    owner: AgentRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Request {
    command: String,
    since: Option<String>,
    owner: Option<AgentRef>,
    baseline: Option<SelectionRef>,
    observation: ObservationRef,
}

pub(super) struct Continuation {
    previous: Option<SelectionRef>,
    request: ObservationRef,
    tail: Option<String>,
}

impl State {
    pub(super) fn observe(&mut self, position: &SourcePosition) {
        let contiguous = self.last_position.as_ref().is_some_and(|last| {
            last.source == position.source && last.generation == position.generation
                && matches!((&last.locator, &position.locator),
                    (SourceLocator::Frame { ordinal: a }, SourceLocator::Frame { ordinal: b }) if a.checked_add(1) == Some(*b))
        });
        if !contiguous {
            self.invalidate();
        }
        self.last_position = Some(position.clone());
    }

    pub(super) fn invalidate(&mut self) {
        self.baseline = None;
        self.pending.clear();
    }

    pub(super) fn request(
        &mut self,
        owner: Option<AgentRef>,
        value: &Value,
        observation: &ObservationRef,
    ) {
        let command = text(value, "type").unwrap_or("");
        if matches!(command, "new_session" | "switch_session" | "fork" | "clone") {
            self.invalidate();
            return;
        }
        if !matches!(command, "get_entries" | "get_tree") {
            return;
        }
        let Some(id) = request_id(value) else {
            self.invalidate();
            return;
        };
        let since = match value.get("since") {
            None => None,
            Some(Value::String(since)) if command == "get_entries" && !since.is_empty() => {
                Some(since.clone())
            }
            _ => {
                self.invalidate();
                return;
            }
        };
        if self.pending.len() >= MAX_PENDING || self.pending.contains_key(&id) {
            // Ambiguous/recycled IDs cannot certify a baseline. A subsequent
            // fresh full request can establish one without growing this table.
            self.invalidate();
            return;
        }
        self.pending.insert(
            id,
            Request {
                command: command.into(),
                since,
                owner,
                baseline: self.baseline.as_ref().map(|base| base.selection.clone()),
                observation: observation.clone(),
            },
        );
    }

    pub(super) fn response(&mut self, owner: &AgentRef, value: &Value) -> Option<Continuation> {
        let request = request_id(value).and_then(|id| self.pending.remove(&id));
        let result = request.and_then(|request| {
            if text(value, "command") != Some(request.command.as_str())
                || value["success"] != true
                || request.owner.as_ref() != Some(owner)
                || request.baseline != self.baseline.as_ref().map(|base| base.selection.clone())
            {
                return None;
            }
            let previous = if let Some(since) = &request.since {
                let baseline = self.baseline.as_ref()?;
                if &baseline.owner != owner || baseline.tail.as_ref() != Some(since) {
                    return None;
                }
                Some(baseline.selection.clone())
            } else {
                None
            };
            let tail = if request.command == "get_entries" {
                let entries = value["data"]["entries"].as_array()?;
                let mut seen = BTreeSet::new();
                for entry in entries {
                    let id = text(entry, "id").filter(|id| !id.is_empty())?;
                    if !seen.insert(id) || request.since.as_deref() == Some(id) {
                        return None;
                    }
                }
                entries
                    .last()
                    .and_then(|entry| text(entry, "id"))
                    .map(str::to_owned)
                    .or(request.since)
            } else {
                value["data"]["tree"].as_array()?;
                // A complete tree snapshot supplies a verified leaf. A later
                // get_entries(since=leaf) may extend it, while tree order itself
                // never becomes an append cursor.
                value["data"]["leafId"].as_str().map(str::to_owned)
            };
            Some(Continuation {
                previous,
                request: request.observation,
                tail,
            })
        });
        if result.is_none() {
            self.invalidate();
        }
        result
    }

    pub(super) fn accept(
        &mut self,
        owner: &AgentRef,
        selection: &SelectionRef,
        proof: &Continuation,
    ) {
        self.baseline = Some(Baseline {
            selection: selection.clone(),
            tail: proof.tail.clone(),
            owner: owner.clone(),
        });
    }
}

impl Continuation {
    pub(super) fn scope(&self, position: &SourcePosition) -> HistoryContentScope {
        HistoryContentScope::Snapshot {
            position: position.clone(),
            mapping_rule: "pi/rpc-entry-history".into(),
            previous: self.previous.clone(),
        }
    }
    pub(super) fn add_evidence(&self, claim: &mut Claim) {
        claim.evidence.push(self.request.clone());
        claim.evidence.sort();
        claim.evidence.dedup();
    }
}

fn request_id(value: &Value) -> Option<String> {
    value
        .get("id")
        .filter(|id| matches!(id, Value::String(_) | Value::Number(_)))
        .map(Value::to_string)
}
