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

use tokio::sync::RwLock;

use crate::db::JournalStore;
use crate::error::Result;
use sylph_protocol::SessionId;

/// Hands out monotonic sequence numbers, seeded from the journal.
#[derive(Clone)]
pub struct SeqAllocator {
    store: JournalStore,
    counters: Arc<RwLock<HashMap<String, Arc<AtomicI32>>>>,
}

impl SeqAllocator {
    pub fn new(store: JournalStore) -> Self {
        Self {
            store,
            counters: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// The next sequence number for a session.
    ///
    /// Seeded from `MAX(seq)` on first use so numbering keeps climbing across a
    /// restart rather than restarting at zero and colliding with journaled rows.
    pub async fn next(&self, session_id: &SessionId) -> Result<i32> {
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
        let first = allocator.next(&crate::fixtures::test_session_id("s1")).await.unwrap();
        let second = allocator.next(&crate::fixtures::test_session_id("s1")).await.unwrap();
        assert_eq!(second, first + 1);
    }

    #[tokio::test]
    async fn sessions_are_numbered_independently() {
        // Ordering is per session, so one busy session must not push another's
        // numbers forward and leave gaps in its stream.
        let (store, _dir) = store().await;
        let allocator = SeqAllocator::new(store);
        allocator.next(&crate::fixtures::test_session_id("s1")).await.unwrap();
        allocator.next(&crate::fixtures::test_session_id("s1")).await.unwrap();
        assert_eq!(allocator.next(&crate::fixtures::test_session_id("s2")).await.unwrap(), 0);
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
                    mine.push(allocator.next(&crate::fixtures::test_session_id("shared")).await.unwrap());
                }
                mine
            }));
        }

        let mut all = Vec::new();
        for handle in handles {
            all.extend(handle.await.unwrap());
        }

        let unique: std::collections::HashSet<_> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len(), "every number must be handed out once");
        // Dense from 0, so the journal has no unexplained gaps.
        let mut sorted: Vec<_> = unique.into_iter().collect();
        sorted.sort();
        assert_eq!(sorted.first(), Some(&0));
        assert_eq!(sorted.last(), Some(&(all.len() as i32 - 1)));
    }
}
