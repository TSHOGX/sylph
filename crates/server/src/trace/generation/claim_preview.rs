//! One bounded diagnostic summary, retained beside each immutable claim.
use super::*;
use serde_json::{Value, json};
use sylph_protocol::trace::Fact;

pub(super) fn preview(claim: &Claim, maximum: usize) -> Result<(String, bool)> {
    let value: Value = match &claim.fact {
        Fact::Declare {
            object: TraceObject::Message(message),
        } => {
            json!({"author":message.author,"purpose":message.purpose,"protocolRole":message.protocol_role})
        }
        Fact::Declare {
            object: TraceObject::Delivery(delivery),
        } => {
            json!({"sender":delivery.sender,"recipient":delivery.recipient,"route":delivery.route})
        }
        Fact::Declare {
            object: TraceObject::Agent(agent),
        } => json!({"agent":agent.agent_ref,"name":agent.name,"model":agent.model}),
        Fact::Declare {
            object: TraceObject::Operation(operation),
        } => json!({"owner":operation.owner,"category":operation.category}),
        Fact::Declare {
            object: TraceObject::Resource(resource),
        } => json!({"kind":resource.kind,"host":resource.host}),
        Fact::Execution { boundary, .. } => serde_json::to_value(boundary)?,
        Fact::ResourceState { state, .. } => serde_json::to_value(state)?,
        Fact::Diagnostic { diagnostic } => {
            json!({"code":diagnostic.code,"message":diagnostic.message})
        }
        _ => serde_json::to_value(&claim.fact)?,
    };
    let text = serde_json::to_string(&value)?;
    // The complete fact is still an explicit bounded claim-detail read. Keeping
    // this fixed summary at publication avoids decoding large facts on every page.
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok((text[..end].into(), end < text.len()))
}
