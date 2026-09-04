use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::sync::Mutex;

use sylph_envagent::{EventSink, Spool, SpoolConfig, SpoolEvent};

#[derive(Default)]
struct FailOnceSink {
    attempts: Mutex<usize>,
    received: Mutex<Vec<u64>>,
}

#[async_trait]
impl EventSink for FailOnceSink {
    async fn deliver(&self, events: &[SpoolEvent]) -> Result<u64> {
        let mut attempts = self.attempts.lock().await;
        *attempts += 1;
        if *attempts == 1 {
            return Err(anyhow!("injected send failure"));
        }
        self.received
            .lock()
            .await
            .extend(events.iter().map(|event| event.producer_seq));
        Ok(events.last().map(|event| event.producer_seq).unwrap_or(0))
    }
}

fn event(seq: u64, payload: &[u8]) -> SpoolEvent {
    SpoolEvent {
        producer_id: "p".into(),
        producer_seq: seq,
        kind: "event".into(),
        session_id: "env7fk2:fixture:n".into(),
        arrival_turn_id: None,
        payload: payload.to_vec(),
        source_cursor: None,
        recorded_at_ms: None,
    }
}

#[tokio::test]
async fn a_send_failure_replays_the_same_prefix_without_loss() {
    let dir = tempfile::tempdir().unwrap();
    let spool = Spool::open(SpoolConfig::new(dir.path().join("spool.db"), 1024))
        .await
        .unwrap();
    spool.append(&event(1, b"one")).await.unwrap();
    spool.append(&event(2, b"two")).await.unwrap();
    let sink = Arc::new(FailOnceSink::default());

    let first = sink.deliver(&spool.pending("p", 16).await.unwrap()).await;
    assert!(first.is_err());
    assert_eq!(spool.pending("p", 16).await.unwrap().len(), 2);

    let pending = spool.pending("p", 16).await.unwrap();
    let ack = sink.deliver(&pending).await.unwrap();
    spool.ack_through("p", ack).await.unwrap();
    assert!(spool.is_empty().await.unwrap());
    assert_eq!(*sink.received.lock().await, vec![1, 2]);
}

#[tokio::test]
async fn a_partial_ack_leaves_the_unacknowledged_suffix_for_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let spool = Spool::open(SpoolConfig::new(dir.path().join("spool.db"), 1024))
        .await
        .unwrap();
    spool.append(&event(1, b"one")).await.unwrap();
    spool.append(&event(2, b"two")).await.unwrap();
    spool.append(&event(3, b"three")).await.unwrap();
    spool.ack_through("p", 2).await.unwrap();
    let pending = spool.pending("p", 16).await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|event| event.producer_seq)
            .collect::<Vec<_>>(),
        vec![3]
    );
}
