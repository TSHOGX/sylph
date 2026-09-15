//! Canonicalize derived references while original claims remain immutable.

use super::*;

fn normalized_owner(registry: &BTreeMap<AnyObjectRef, AnyObjectRef>, owner: &OwnerRef) -> OwnerRef {
    match owner {
        OwnerRef::Agent { agent } => OwnerRef::Agent {
            agent: resolved_ref!(registry, Agent, agent),
        },
        other => other.clone(),
    }
}

fn normalized_author(registry: &BTreeMap<AnyObjectRef, AnyObjectRef>, author: &Author) -> Author {
    match author {
        Author::Agent { agent } => Author::Agent {
            agent: normalized_owner(registry, agent),
        },
        Author::Tool { operation } => Author::Tool {
            operation: operation
                .as_ref()
                .map(|reference| resolved_ref!(registry, Operation, reference)),
        },
        other => other.clone(),
    }
}

fn normalized_recipient(
    registry: &BTreeMap<AnyObjectRef, AnyObjectRef>,
    recipient: &Recipient,
) -> Recipient {
    match recipient {
        Recipient::Agent { agent } => Recipient::Agent {
            agent: normalized_owner(registry, agent),
        },
        Recipient::Operation { operation } => Recipient::Operation {
            operation: resolved_ref!(registry, Operation, operation),
        },
        other => other.clone(),
    }
}

fn normalized_history_content(
    registry: &BTreeMap<AnyObjectRef, AnyObjectRef>,
    content: &HistoryContent,
) -> HistoryContent {
    match content {
        HistoryContent::Message(reference) => {
            HistoryContent::Message(resolved_ref!(registry, MessageRevision, reference))
        }
        HistoryContent::Operation(reference) => {
            HistoryContent::Operation(resolved_ref!(registry, Operation, reference))
        }
        HistoryContent::Checkpoint(reference) => {
            HistoryContent::Checkpoint(resolved_ref!(registry, ContextCheckpoint, reference))
        }
        other => other.clone(),
    }
}

fn normalized_object(
    registry: &BTreeMap<AnyObjectRef, AnyObjectRef>,
    object: &TraceObject,
) -> TraceObject {
    let mut object = object.clone();
    match &mut object {
        TraceObject::Agent(v) => v.agent_ref = resolved_ref!(registry, Agent, v.agent_ref),
        TraceObject::Activation(v) => {
            v.activation_ref = resolved_ref!(registry, Activation, v.activation_ref);
            v.owner = normalized_owner(registry, &v.owner);
        }
        TraceObject::Message(v) => {
            v.message_ref = resolved_ref!(registry, Message, v.message_ref);
            v.author = normalized_author(registry, &v.author);
            if let Some(p) = &mut v.production {
                p.owner = normalized_owner(registry, &p.owner);
                p.activation = p
                    .activation
                    .as_ref()
                    .map(|r| resolved_ref!(registry, Activation, r));
            }
        }
        TraceObject::MessageRevision(v) => {
            v.revision_ref = resolved_ref!(registry, MessageRevision, v.revision_ref);
            v.message = resolved_ref!(registry, Message, v.message);
            match &mut v.form {
                RevisionForm::Append { predecessor } => {
                    *predecessor = predecessor
                        .as_ref()
                        .map(|r| resolved_ref!(registry, MessageRevision, r))
                }
                RevisionForm::Correction { replaces } => {
                    *replaces = resolved_ref!(registry, MessageRevision, replaces)
                }
                _ => {}
            }
        }
        TraceObject::Delivery(v) => {
            v.delivery_ref = resolved_ref!(registry, Delivery, v.delivery_ref);
            v.sender = normalized_author(registry, &v.sender);
            v.recipient = normalized_recipient(registry, &v.recipient);
            v.message_revision = resolved_ref!(registry, MessageRevision, v.message_revision);
        }
        TraceObject::Operation(v) => {
            v.operation_ref = resolved_ref!(registry, Operation, v.operation_ref);
            v.owner = normalized_owner(registry, &v.owner);
            v.activation = v
                .activation
                .as_ref()
                .map(|r| resolved_ref!(registry, Activation, r));
            v.requester = v.requester.as_ref().map(|r| match r {
                RequesterRef::Agent(v) => RequesterRef::Agent(resolved_ref!(registry, Agent, v)),
                RequesterRef::Activation(v) => {
                    RequesterRef::Activation(resolved_ref!(registry, Activation, v))
                }
                RequesterRef::Operation(v) => {
                    RequesterRef::Operation(resolved_ref!(registry, Operation, v))
                }
                RequesterRef::Delivery(v) => {
                    RequesterRef::Delivery(resolved_ref!(registry, Delivery, v))
                }
                RequesterRef::Resource(v) => {
                    RequesterRef::Resource(resolved_ref!(registry, Resource, v))
                }
            });
        }
        TraceObject::Result(v) => {
            v.result_ref = resolved_ref!(registry, Result, v.result_ref);
            v.operation = resolved_ref!(registry, Operation, v.operation);
            v.recipient = v
                .recipient
                .as_ref()
                .map(|r| normalized_recipient(registry, r));
            v.delivery = v
                .delivery
                .as_ref()
                .map(|r| resolved_ref!(registry, Delivery, r));
        }
        TraceObject::Resource(v) => {
            v.resource_ref = resolved_ref!(registry, Resource, v.resource_ref);
            v.creator = v
                .creator
                .as_ref()
                .map(|r| resolved_ref!(registry, Operation, r));
            v.host = v.host.as_ref().map(|r| normalized_owner(registry, r));
        }
        TraceObject::HistoryEntry(v) => {
            v.entry_ref = resolved_ref!(registry, HistoryEntry, v.entry_ref);
            v.containing_agent = resolved_ref!(registry, Agent, v.containing_agent);
            v.content = v
                .content
                .iter()
                .map(|r| normalized_history_content(registry, r))
                .collect();
            v.predecessor = v
                .predecessor
                .as_ref()
                .map(|r| resolved_ref!(registry, HistoryEntry, r));
            v.original_owner = v
                .original_owner
                .as_ref()
                .map(|r| normalized_owner(registry, r));
        }
        TraceObject::HistorySelection(v) => {
            v.selection_ref = resolved_ref!(registry, HistorySelection, v.selection_ref);
            v.agent = resolved_ref!(registry, Agent, v.agent);
            match &mut v.membership {
                SelectionMembership::EntryPath { head } => {
                    *head = resolved_ref!(registry, HistoryEntry, head)
                }
                SelectionMembership::EntrySet { entries } => {
                    *entries = entries
                        .iter()
                        .map(|r| resolved_ref!(registry, HistoryEntry, r))
                        .collect()
                }
                SelectionMembership::SourceExtents {
                    inherited_selections,
                    ..
                } => {
                    *inherited_selections = inherited_selections
                        .iter()
                        .map(|r| resolved_ref!(registry, HistorySelection, r))
                        .collect()
                }
            }
        }
        TraceObject::ContextCheckpoint(v) => {
            v.checkpoint_ref = resolved_ref!(registry, ContextCheckpoint, v.checkpoint_ref);
            v.owner = normalized_owner(registry, &v.owner);
            match &mut v.content {
                ContextContent::Ordered { entries, .. } => {
                    *entries = entries
                        .iter()
                        .map(|r| normalized_history_content(registry, r))
                        .collect()
                }
                ContextContent::Selection { selection } => {
                    *selection = resolved_ref!(registry, HistorySelection, selection)
                }
                _ => {}
            }
        }
        TraceObject::Measurement(v) => {
            v.measurement_ref = resolved_ref!(registry, Measurement, v.measurement_ref);
            v.subject = canonical(registry, &v.subject);
            for component in &mut v.components {
                component.measurement = resolved_ref!(registry, Measurement, component.measurement);
            }
        }
    }
    object
}

fn normalized_relation(
    registry: &BTreeMap<AnyObjectRef, AnyObjectRef>,
    relation: &Relation,
) -> Relation {
    match relation {
        Relation::Spawn {
            parent,
            child,
            operation,
        } => Relation::Spawn {
            parent: resolved_ref!(registry, Agent, parent),
            child: resolved_ref!(registry, Agent, child),
            operation: operation
                .as_ref()
                .map(|r| resolved_ref!(registry, Operation, r)),
        },
        Relation::Interaction {
            sender,
            recipient,
            delivery,
            operation,
        } => Relation::Interaction {
            sender: resolved_ref!(registry, Agent, sender),
            recipient: resolved_ref!(registry, Agent, recipient),
            delivery: delivery
                .as_ref()
                .map(|r| resolved_ref!(registry, Delivery, r)),
            operation: operation
                .as_ref()
                .map(|r| resolved_ref!(registry, Operation, r)),
        },
        Relation::Trigger {
            delivery,
            activation,
        } => Relation::Trigger {
            delivery: resolved_ref!(registry, Delivery, delivery),
            activation: resolved_ref!(registry, Activation, activation),
        },
        Relation::Nesting { enclosing, nested } => Relation::Nesting {
            enclosing: resolved_ref!(registry, Operation, enclosing),
            nested: resolved_ref!(registry, Operation, nested),
        },
        Relation::UsesResource {
            operation,
            resource,
        } => Relation::UsesResource {
            operation: resolved_ref!(registry, Operation, operation),
            resource: resolved_ref!(registry, Resource, resource),
        },
        Relation::HistoryPredecessor { predecessor, entry } => Relation::HistoryPredecessor {
            predecessor: resolved_ref!(registry, HistoryEntry, predecessor),
            entry: resolved_ref!(registry, HistoryEntry, entry),
        },
        Relation::ContextLineage {
            recipient,
            checkpoint,
            original_owner,
        } => Relation::ContextLineage {
            recipient: resolved_ref!(registry, Agent, recipient),
            checkpoint: resolved_ref!(registry, ContextCheckpoint, checkpoint),
            original_owner: normalized_owner(registry, original_owner),
        },
        Relation::Representation {
            original,
            transformed,
            rule,
        } => Relation::Representation {
            original: resolved_ref!(registry, MessageRevision, original),
            transformed: resolved_ref!(registry, MessageRevision, transformed),
            rule: rule.clone(),
        },
        Relation::ContextMembership {
            checkpoint,
            selection,
            disposition,
        } => Relation::ContextMembership {
            checkpoint: resolved_ref!(registry, ContextCheckpoint, checkpoint),
            selection: resolved_ref!(registry, HistorySelection, selection),
            disposition: *disposition,
        },
        Relation::ReadingOrder { before, after } => Relation::ReadingOrder {
            before: resolved_ref!(registry, HistoryEntry, before),
            after: resolved_ref!(registry, HistoryEntry, after),
        },
    }
}

pub(super) fn normalized_claim(
    registry: &BTreeMap<AnyObjectRef, AnyObjectRef>,
    claim: &Claim,
) -> Claim {
    let mut claim = claim.clone();
    match &mut claim.fact {
        Fact::Declare { object } => *object = normalized_object(registry, object),
        Fact::AgentState { agent, .. } => *agent = resolved_ref!(registry, Agent, agent),
        Fact::DeliveryStage { delivery, stage } => {
            *delivery = resolved_ref!(registry, Delivery, delivery);
            match stage {
                DeliveryStage::Admitted { entry } => {
                    *entry = entry
                        .as_ref()
                        .map(|r| resolved_ref!(registry, HistoryEntry, r))
                }
                DeliveryStage::Included { target, .. } => match target {
                    InclusionTarget::Inference { operation } => {
                        *operation = resolved_ref!(registry, Operation, operation)
                    }
                    InclusionTarget::Checkpoint { checkpoint } => {
                        *checkpoint = resolved_ref!(registry, ContextCheckpoint, checkpoint)
                    }
                },
                _ => {}
            }
        }
        Fact::Execution { execution, .. } => {
            *execution = match execution {
                ExecutionRef::Activation(v) => {
                    ExecutionRef::Activation(resolved_ref!(registry, Activation, v))
                }
                ExecutionRef::Operation(v) => {
                    ExecutionRef::Operation(resolved_ref!(registry, Operation, v))
                }
                ExecutionRef::Resource(v) => {
                    ExecutionRef::Resource(resolved_ref!(registry, Resource, v))
                }
            }
        }
        Fact::ResourceState { resource, .. } => {
            *resource = resolved_ref!(registry, Resource, resource)
        }
        Fact::Relation { relation } => *relation = normalized_relation(registry, relation),
        Fact::SelectHistory {
            agent, selection, ..
        } => {
            *agent = resolved_ref!(registry, Agent, agent);
            *selection = resolved_ref!(registry, HistorySelection, selection);
        }
        Fact::HistoryPosition { entry, .. } => {
            *entry = resolved_ref!(registry, HistoryEntry, entry)
        }
        Fact::HistoryMappingComplete { extent } => {
            extent.original_owner = extent
                .original_owner
                .as_ref()
                .map(|owner| normalized_owner(registry, owner))
        }
        Fact::Equivalence { left, right, .. } => {
            *left = canonical(registry, left);
            *right = canonical(registry, right);
        }
        Fact::Coverage { coverage } => coverage.subject = canonical(registry, &coverage.subject),
        Fact::Diagnostic { diagnostic } => {
            diagnostic.subject = diagnostic
                .subject
                .as_ref()
                .map(|subject| canonical(registry, subject))
        }
    }
    claim
}
