//! Deterministic transport fixture used by protocol failure-injection tests.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::runtime::EventSink;
use crate::spool::SpoolEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    Send,
    Commit,
    Ack,
    Delete,
}

#[derive(Debug, Clone)]
pub struct FaultPlan {
    point: Option<FaultPoint>,
    remaining: usize,
}

impl FaultPlan {
    pub fn fail_once(point: FaultPoint) -> Self {
        Self {
            point: Some(point),
            remaining: 1,
        }
    }

    pub fn none() -> Self {
        Self {
            point: None,
            remaining: 0,
        }
    }

    fn take(&mut self, point: FaultPoint) -> bool {
        if self.point == Some(point) && self.remaining > 0 {
            self.remaining -= 1;
            true
        } else {
            false
        }
    }
}

#[derive(Clone)]
pub struct FixtureSink {
    plan: Arc<Mutex<FaultPlan>>,
    received: Arc<Mutex<Vec<SpoolEvent>>>,
}

impl FixtureSink {
    pub fn new(plan: FaultPlan) -> Self {
        Self {
            plan: Arc::new(Mutex::new(plan)),
            received: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn received(&self) -> Vec<SpoolEvent> {
        self.received.lock().await.clone()
    }
}

#[async_trait]
impl EventSink for FixtureSink {
    async fn deliver(&self, events: &[SpoolEvent]) -> Result<u64> {
        if self.plan.lock().await.take(FaultPoint::Send) {
            return Err(anyhow!("fixture fault at send"));
        }
        self.received.lock().await.extend_from_slice(events);
        if self.plan.lock().await.take(FaultPoint::Ack) {
            return Err(anyhow!("fixture fault before ack"));
        }
        Ok(events.last().map(|event| event.producer_seq).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fixture_fault_is_consumed_once_and_the_second_delivery_succeeds() {
        let sink = FixtureSink::new(FaultPlan::fail_once(FaultPoint::Send));
        let row = SpoolEvent {
            producer_id: "p".into(),
            producer_seq: 1,
            kind: "event".into(),
            session_id: "env7fk2:fixture:n".into(),
            arrival_turn_id: None,
            payload: b"x".to_vec(),
            source_cursor: None,
            recorded_at_ms: None,
        };
        assert!(sink.deliver(std::slice::from_ref(&row)).await.is_err());
        assert_eq!(sink.deliver(std::slice::from_ref(&row)).await.unwrap(), 1);
        assert_eq!(sink.received().await.len(), 1);
    }
}
