//! One sequence-number allocator per session, shared by every producer.
//!
//! Sequence numbers order a session's whole event stream and back
//! `UNIQUE(session_id, seq)` in the journal, so two producers cannot each keep
//! their own counter. There are two: the turn supervisor emits server-authored
//! events (the user's message, status, errors) and a link's drain task emits the
//! agent's. Separate counters would hand out the same number and one of the two
//! writes would fail — intermittently, under concurrency, which is the worst way
//! to find out.
//!
//! Deriving the next number from `MAX(seq)` per event instead would be racy for
//! the same reason and cost a database read on the hot path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};

use crate::db::JournalStore;
use crate::error::Result;
use sylph_protocol::SessionId;

/// Hands out monotonic sequence numbers, seeded from the journal.
#[derive(Clone)]
pub struct SeqAllocator {
    store: JournalStore,
    counters: Arc<RwLock<HashMap<String, Arc<AtomicI32>>>>,
    /// Serializes in-memory reservations with bridge transactions that derive
    /// canonical sequence values from SQLite.
    gate: Arc<Mutex<()>>,
}

impl SeqAllocator {
    pub fn new(store: JournalStore) -> Self {
        Self {
            store,
            counters: Arc::new(RwLock::new(HashMap::new())),
            gate: Arc::new(Mutex::new(())),
        }
    }

    /// Shared reservation gate. A bridge ingest holds this through its SQLite
    /// transaction and cursor update; server producers acquire it in `next`.
    pub(crate) fn gate(&self) -> Arc<Mutex<()>> {
        self.gate.clone()
    }

    /// The next sequence number for a session.
    ///
    /// Seeded from `MAX(seq)` on first use so numbering keeps climbing across a
    /// restart rather than restarting at zero and colliding with journaled rows.
    pub async fn next(&self, session_id: &SessionId) -> Result<i32> {
        let _gate = self.gate.lock().await;
        self.next_inner(session_id).await
    }

    async fn next_inner(&self, session_id: &SessionId) -> Result<i32> {
        // Keyed on the encoded triple rather than on `SessionId` itself: the map is
        // hot — one lookup per event — and the encoding has a single producer, so the
        // two spellings cannot diverge.
        let key = session_id.encode();
        let counter = {
            let counters = self.counters.read().await;
            counters.get(&key).cloned()
        };

        let counter = match counter {
            Some(counter) => counter,
            None => {
                let max = self.store.max_event_seq(session_id).await?;
                let mut counters = self.counters.write().await;
                // Another task may have seeded this while we queried. Keeping
                // whichever landed first is what keeps numbering monotonic — the
                // loser's seed would rewind the counter.
                counters
                    .entry(key)
                    .or_insert_with(|| Arc::new(AtomicI32::new(max + 1)))
                    .clone()
            }
        };

        Ok(counter.fetch_add(1, Ordering::SeqCst))
    }

    /// Advance the next value after a sequence allocated by another producer.
    ///
    /// Envagent events receive their canonical sequence inside the ingest
    /// transaction, so they do not call [`Self::next`].  The allocator is shared
    /// with server-authored events, however; leaving its counter unchanged would
    /// make the next status or error reuse a number the bridge has just journaled.
    /// Raise the counter monotonically and never rewind a value already handed out.
    pub async fn observe(&self, session_id: &SessionId, next_seq: i32) {
        let _gate = self.gate.lock().await;
        self.observe_inner(session_id, next_seq).await;
    }

    /// Advance the cursor while the caller already holds [`Self::gate`].
    pub(crate) async fn observe_inner(&self, session_id: &SessionId, next_seq: i32) {
        let key = session_id.encode();
        let counter = {
            let counters = self.counters.read().await;
            counters.get(&key).cloned()
        };
        let counter = match counter {
            Some(counter) => counter,
            None => {
                let max = self.store.max_event_seq(session_id).await.unwrap_or(-1);
                let mut counters = self.counters.write().await;
                counters
                    .entry(key)
                    .or_insert_with(|| Arc::new(AtomicI32::new(max + 1)))
                    .clone()
            }
        };

        let mut current = counter.load(Ordering::SeqCst);
        while current < next_seq {
            match counter.compare_exchange(current, next_seq, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => break,
                Err(updated) => current = updated,
            }
        }
    }

    /// Forget an optimistic in-memory counter after a transaction fails.
    ///
    /// The next allocation is reseeded from the durable journal, so a failed
    /// envagent ingest cannot permanently consume a canonical sequence.
    pub async fn reset_session(&self, session_id: &SessionId) {
        self.counters.write().await.remove(&session_id.encode());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file-backed store, because migrations run on connect and `:memory:`
    /// would leave the allocator querying a table that does not exist. The
    /// tempdir is returned so it outlives the store — dropping it deletes the
    /// database out from under the pool.
    async fn store() -> (JournalStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = JournalStore::new(dir.path().join("journal.db").to_str().unwrap())
            .await
            .expect("store");
        (store, dir)
    }

    #[tokio::test]
    async fn numbers_climb_and_never_repeat_for_one_session() {
        let (store, _dir) = store().await;
        let allocator = SeqAllocator::new(store);
        let first = allocator
            .next(&crate::fixtures::test_session_id("s1"))
            .await
            .unwrap();
        let second = allocator
            .next(&crate::fixtures::test_session_id("s1"))
            .await
            .unwrap();
        assert_eq!(second, first + 1);
    }

    #[tokio::test]
    async fn sessions_are_numbered_independently() {
        // Ordering is per session, so one busy session must not push another's
        // numbers forward and leave gaps in its stream.
        let (store, _dir) = store().await;
        let allocator = SeqAllocator::new(store);
        allocator
            .next(&crate::fixtures::test_session_id("s1"))
            .await
            .unwrap();
        allocator
            .next(&crate::fixtures::test_session_id("s1"))
            .await
            .unwrap();
        assert_eq!(
            allocator
                .next(&crate::fixtures::test_session_id("s2"))
                .await
                .unwrap(),
            0
        );
    }

    /// The property the shared allocator exists for: concurrent producers never
    /// receive the same number. A per-producer counter fails this.
    #[tokio::test]
    async fn concurrent_producers_never_receive_the_same_number() {
        let (store, _dir) = store().await;
        let allocator = SeqAllocator::new(store);

        let mut handles = Vec::new();
        for _ in 0..8 {
            let allocator = allocator.clone();
            handles.push(tokio::spawn(async move {
                let mut mine = Vec::new();
                for _ in 0..50 {
                    mine.push(
                        allocator
                            .next(&crate::fixtures::test_session_id("shared"))
                            .await
                            .unwrap(),
                    );
                }
                mine
            }));
        }

        let mut all = Vec::new();
        for handle in handles {
            all.extend(handle.await.unwrap());
        }

        let unique: std::collections::HashSet<_> = all.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all.len(),
            "every number must be handed out once"
        );
        // Dense from 0, so the journal has no unexplained gaps.
        let mut sorted: Vec<_> = unique.into_iter().collect();
        sorted.sort();
        assert_eq!(sorted.first(), Some(&0));
        assert_eq!(sorted.last(), Some(&(all.len() as i32 - 1)));
    }

    #[tokio::test]
    async fn observing_remote_sequence_advances_server_producer() {
        let (store, _dir) = store().await;
        let allocator = SeqAllocator::new(store);
        let session = crate::fixtures::test_session_id("remote");

        assert_eq!(allocator.next(&session).await.unwrap(), 0);
        allocator.observe(&session, 8).await;
        assert_eq!(allocator.next(&session).await.unwrap(), 8);

        // A late remote event must never rewind a value already handed out.
        allocator.observe(&session, 3).await;
        assert_eq!(allocator.next(&session).await.unwrap(), 9);
    }

    #[tokio::test]
    async fn reservation_gate_blocks_server_sequence_until_remote_commit() {
        let (store, _dir) = store().await;
        let allocator = SeqAllocator::new(store);
        let session = crate::fixtures::test_session_id("remote-gate");
        let gate = allocator.gate();
        let guard = gate.lock().await;
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let pending = {
            let allocator = allocator.clone();
            let session = session.clone();
            tokio::spawn(async move {
                let value = allocator.next(&session).await.unwrap();
                let _ = tx.send(value);
            })
        };
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err());
        drop(guard);
        assert_eq!(rx.await.unwrap(), 0);
        pending.await.unwrap();
    }
}
