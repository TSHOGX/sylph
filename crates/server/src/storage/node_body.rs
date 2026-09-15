//! Resolve one projection body across the archive publication boundary.
use super::HistoryStore;
use crate::db::timeline::Node;
use crate::error::{Error, Result};
use crate::models::Event;
use sylph_protocol::SessionId;

pub(crate) async fn resolve(
    history: &HistoryStore,
    session: &SessionId,
    node: &Node,
    hot: Vec<Event>,
    seqs: Vec<i32>,
) -> Result<serde_json::Value> {
    if node.event.body_ref.is_none() {
        return Ok(serde_json::json!({"text":node.event.text,"data":node.event.data}));
    }
    let hot_seqs = hot
        .iter()
        .map(|event| event.seq)
        .collect::<std::collections::HashSet<_>>();
    let archived = seqs
        .into_iter()
        .filter(|seq| !hot_seqs.contains(seq))
        .collect::<Vec<_>>();
    let blocks = history
        .node_body_blocks(&session.encode(), &node.event.kind, &archived)
        .await
        .map_err(|e| Error::Internal(format!("could not read node body: {e:#}")))?;
    let mut parts = std::collections::BTreeMap::new();
    for block in blocks {
        let replace = block.phase.as_deref() == Some("completed") || block.kind == "subagent";
        parts.insert(block.seq, (replace, block.text, block.data));
    }
    for event in hot {
        let replace = event.phase.as_deref() != Some("delta") || event.kind == "subagent";
        parts.insert(
            event.seq,
            (
                replace,
                event.text,
                crate::events::data_from_column(&event.data_json),
            ),
        );
    }
    let mut text = String::new();
    let mut data = None;
    for (replace, part, value) in parts.into_values() {
        if replace {
            text = part;
            data = value;
        } else {
            text.push_str(&part);
            if value.is_some() {
                data = value;
            }
        }
    }
    let expected = node.event.body_ref.as_ref().unwrap().text_bytes;
    if text.len() as u64 != expected {
        return Err(Error::Internal(
            "node body is incomplete; a retained fragment is unavailable".into(),
        ));
    }
    let data_bytes = data
        .as_ref()
        .map_or(0, |value| value.to_string().len() as u64);
    if data_bytes != node.event.body_ref.as_ref().unwrap().data_bytes {
        return Err(Error::Internal(
            "node metadata is incomplete; a retained fragment is unavailable".into(),
        ));
    }
    Ok(serde_json::json!({"text":text,"data":data}))
}
