//! A short-lived snapshot of what a machine's native stores hold.
//!
//! # Why paging needs this, measured
//!
//! `sylph_protocol::Adapter::enumerate` is required to answer without parsing every
//! session, and it does. It is still not free, and the contract's own figure is not what
//! this machine reports — measured 2026-08-29 against the real stores, one unfiltered
//! listing pass each:
//!
//! | Agent | What it reads per file | Relative cost |
//! | --- | --- | --- |
//! | `pi` | one header | cheapest |
//! | `claude` | a 32 KB head, for the vendor's own title | ~20× `pi` |
//! | `codex` | a header **and a 256 KB tail**, for the last stamp | dearest |
//!
//! So a whole-machine listing costs **seconds**, not the sub-second figure
//! `sylph_protocol::Adapter::enumerate` quotes. That figure is not wrong about the shape
//! — every one of these is a bounded read per file rather than a parse, which is the
//! requirement — it is describing the header read alone, and two of the three adapters
//! deliberately read more than a header to avoid reporting an absent title or an absent
//! end stamp. `codex`'s tail read is ~322 MB of I/O over the store, and its own comment
//! records the choice.
//!
//! `GET /sessions/enumerate` pages, because a picker's first screen is twenty rows and a
//! whole-machine page is 736 KiB. Re-enumerating per page would make walking `codex`
//! alone seven pages at ~4 s each.
//!
//! # It is also what makes paging *correct*
//!
//! This is the part that would matter even if enumeration were instant. A cursor here is
//! an **offset**, because `StoreFilter` has no cursor to pass down and `since_ms` runs the
//! wrong way for a newest-first listing. Offsets into a *live* store are unstable: these
//! files are being written while a user pages, and a session that gains activity between
//! page one and page two moves to the front — pushing a row the user has not seen yet
//! across the boundary, so it is skipped, while another is served twice. Both are silent.
//!
//! Paging against one snapshot removes that. The snapshot is what the cursor indexes
//! into, so a walk is self-consistent by construction rather than by luck.
//!
//! # What is deliberately *not* here
//!
//! No invalidation, no watch, and no persistence. This is a cache with a deadline, and
//! the deadline is short enough that "it may be up to 30 seconds stale" is the whole of
//! its contract. A store-watching invalidation would be a second mechanism for following
//! a native store, and the adapter already owns that one (`Adapter::watch`).
//!
//! It is **not** a store of record either. Nothing reads it to decide anything durable:
//! `adopt` re-reads the store itself, because a row a user clicked 20 seconds ago must not
//! be what decides where an agent runs.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use sylph_protocol::NativeSummary;

/// How long a snapshot may be served.
///
/// Long enough to page through a large store without re-enumerating, short enough that a
/// session started in a terminal appears on a reopened picker. A user who wants it *now*
/// reopens the picker past this, which is the interaction that already means "refresh".
const TTL: Duration = Duration::from_secs(30);

/// How many distinct listings to keep.
///
/// Small on purpose: the keys are (environment, agent, workdir filter) and a user pages
/// through one at a time. This bounds memory to a few listings rather than to every
/// filter a client has ever sent — a whole machine's summaries measure well under a
/// megabyte serialized, so an
/// unbounded map is a real leak on a machine with a large store and a busy filter input.
const MAX_ENTRIES: usize = 8;

/// One agent's listing, and when it was taken.
struct Entry {
    summaries: Arc<Vec<NativeSummary>>,
    taken: Instant,
}

/// Per-(environment, agent, workdir) listings, each valid for `TTL`.
///
/// Cheap to clone; every clone shares one map.
#[derive(Clone, Default)]
pub struct NativeIndex {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

impl NativeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached listing for this key, or `None` when there is none fresh enough.
    ///
    /// Returns an `Arc` rather than a clone of the vector: a listing of that size is copied
    /// per page otherwise, which is the cost this exists to avoid reappearing as an
    /// allocation.
    pub async fn get(&self, key: &str) -> Option<Arc<Vec<NativeSummary>>> {
        let entries = self.entries.lock().await;
        let entry = entries.get(key)?;
        (entry.taken.elapsed() < TTL).then(|| Arc::clone(&entry.summaries))
    }

    /// Store a listing under this key, evicting the oldest if the map is full.
    pub async fn put(&self, key: String, summaries: Arc<Vec<NativeSummary>>) {
        let mut entries = self.entries.lock().await;

        // Expired entries first, so eviction only ever runs on live ones.
        entries.retain(|_, entry| entry.taken.elapsed() < TTL);

        if entries.len() >= MAX_ENTRIES && !entries.contains_key(&key) {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.taken)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }

        entries.insert(
            key,
            Entry {
                summaries,
                taken: Instant::now(),
            },
        );
    }

    /// The cache key for one agent's listing under one filter.
    ///
    /// The workdir is part of it because a filtered listing is a different listing, and
    /// serving an unfiltered snapshot for a filtered request would return sessions from
    /// directories the caller excluded. `\u{1f}` as the separator: a unit separator cannot
    /// occur in an agent name (the identity encoding forbids the delimiter and a name is
    /// non-empty) and cannot occur in a path on any platform this runs on, so two
    /// different keys cannot collide by concatenation.
    pub fn key(environment: &str, agent: &str, workdir: Option<&str>) -> String {
        format!("{environment}\u{1f}{agent}\u{1f}{}", workdir.unwrap_or(""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylph_protocol::{Fidelity, NativeId};

    fn summary(native: &str) -> NativeSummary {
        NativeSummary {
            native_id: NativeId::new(native),
            workdir: None,
            title: None,
            started_at_ms: None,
            ended_at_ms: None,

            source_revision: None,
            parent_native_id: None,
            parent_relation: None,
            fidelity: Fidelity::Blocks,
        }
    }

    #[tokio::test]
    async fn a_stored_listing_is_served_back_under_its_own_key() {
        let index = NativeIndex::new();
        let key = NativeIndex::key("local", "claude", None);
        index.put(key.clone(), Arc::new(vec![summary("a")])).await;

        let served = index.get(&key).await.expect("fresh entry");
        assert_eq!(served.len(), 1);
        // A different agent is a different listing, never a fallback to this one.
        assert!(index
            .get(&NativeIndex::key("local", "codex", None))
            .await
            .is_none());
    }

    /// A filtered listing must not be served for an unfiltered request, or a picker shows
    /// sessions from directories the caller excluded.
    #[tokio::test]
    async fn a_workdir_filter_is_part_of_the_identity_of_a_listing() {
        let index = NativeIndex::new();
        index
            .put(
                NativeIndex::key("local", "pi", Some("/w/app")),
                Arc::new(vec![summary("a")]),
            )
            .await;

        assert!(index
            .get(&NativeIndex::key("local", "pi", None))
            .await
            .is_none());
        assert!(index
            .get(&NativeIndex::key("local", "pi", Some("/w/app-old")))
            .await
            .is_none());
        assert!(index
            .get(&NativeIndex::key("local", "pi", Some("/w/app")))
            .await
            .is_some());
    }

    /// The map is bounded, or a busy filter input leaks a listing per keystroke —
    /// ~736 KiB serialized per whole-machine listing on a real store.
    #[tokio::test]
    async fn the_map_is_bounded_and_evicts_the_oldest() {
        let index = NativeIndex::new();
        for n in 0..MAX_ENTRIES + 4 {
            index
                .put(
                    NativeIndex::key("local", "claude", Some(&format!("/w/{n}"))),
                    Arc::new(vec![summary("a")]),
                )
                .await;
        }
        assert_eq!(index.entries.lock().await.len(), MAX_ENTRIES);
        // The newest survives; the first is gone.
        assert!(index
            .get(&NativeIndex::key("local", "claude", Some("/w/11")))
            .await
            .is_some());
        assert!(index
            .get(&NativeIndex::key("local", "claude", Some("/w/0")))
            .await
            .is_none());
    }

    /// Re-storing under a live key replaces rather than evicting something else.
    #[tokio::test]
    async fn refreshing_one_key_does_not_evict_another() {
        let index = NativeIndex::new();
        let a = NativeIndex::key("local", "claude", None);
        let b = NativeIndex::key("local", "pi", None);
        index.put(a.clone(), Arc::new(vec![summary("a")])).await;
        index.put(b.clone(), Arc::new(vec![summary("b")])).await;
        index
            .put(a.clone(), Arc::new(vec![summary("a2"), summary("a3")]))
            .await;

        assert_eq!(index.get(&a).await.expect("replaced").len(), 2);
        assert!(
            index.get(&b).await.is_some(),
            "an unrelated key must survive"
        );
    }
}
