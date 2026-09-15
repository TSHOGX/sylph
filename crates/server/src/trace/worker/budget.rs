//! Streaming structural admission bounds before a vendor mapper can expand JSON.

use super::WorkerLimits;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    Malformed,
    ResourceLimit,
}

struct Budget<'a> {
    limits: &'a WorkerLimits,
    nodes: usize,
    exceeded: bool,
}
struct Seed<'a, 'b> {
    budget: &'a mut Budget<'b>,
    depth: usize,
    identity: bool,
}

pub(super) fn validate(raw: &[u8], limits: &WorkerLimits) -> Result<(), Failure> {
    let mut budget = Budget {
        limits,
        nodes: 0,
        exceeded: false,
    };
    let mut decoder = serde_json::Deserializer::from_slice(raw);
    let decoded = Seed {
        budget: &mut budget,
        depth: 0,
        identity: false,
    }
    .deserialize(&mut decoder)
    .and_then(|()| decoder.end());
    if budget.exceeded {
        Err(Failure::ResourceLimit)
    } else if decoded.is_err() {
        Err(Failure::Malformed)
    } else {
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for Seed<'_, '_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        self.budget.nodes += 1;
        if self.budget.nodes > self.budget.limits.max_json_nodes
            || self.depth > self.budget.limits.max_json_depth
        {
            self.budget.exceeded = true;
            return Err(serde::de::Error::custom("semantic JSON budget exhausted"));
        }
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_, '_> {
    type Value = ();
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON value")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<(), E> {
        if self.identity && value.len() > self.budget.limits.max_identity_bytes {
            self.budget.exceeded = true;
            Err(E::custom("identity field exceeds semantic budget"))
        } else {
            Ok(())
        }
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        while sequence
            .next_element_seed(Seed {
                budget: self.budget,
                depth: self.depth + 1,
                identity: self.identity,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > self.budget.limits.max_identity_bytes {
                self.budget.exceeded = true;
                return Err(serde::de::Error::custom(
                    "JSON field name exceeds semantic budget",
                ));
            }
            let identity = key.ends_with("_id")
                || key.ends_with("Id")
                || key.ends_with("ID")
                || key.ends_with("Uuid")
                || matches!(
                    key.as_str(),
                    "id" | "uuid"
                        | "session_id"
                        | "sessionId"
                        | "thread_id"
                        | "threadId"
                        | "agentId"
                        | "agent_id"
                        | "agent_thread_id"
                        | "turn_id"
                        | "turnId"
                        | "parent_thread_id"
                        | "parentAgentId"
                        | "parentUuid"
                        | "parentId"
                        | "call_id"
                        | "tool_use_id"
                        | "toolCallId"
                        | "namespace"
                        | "type"
                        | "role"
                        | "model"
                        | "name"
                );
            map.next_value_seed(Seed {
                budget: self.budget,
                depth: self.depth + 1,
                identity,
            })?;
        }
        Ok(())
    }
}
