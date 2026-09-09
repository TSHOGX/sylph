//! Durable, replayable history.
//!
//! `docs/storage.md` is the governing spec. Three tiers, because there are three
//! workloads with opposite optimal shapes:
//!
//! * [`fold`](crate::storage::fold) — a block's deltas as offsets and timings, so lossless replay costs
//!   ~8 bytes per delta rather than a row.
//! * [`cas`](crate::storage::cas) — content-addressed blobs, so a file an agent reads in twenty turns
//!   is stored once.
//! * [`history`](crate::storage::history) — the read-mostly store the journal consolidates into.
//! * [`retention`](crate::storage::retention) — watermark-driven pruning, never time-driven.
//! * [`scheduler`](crate::storage::scheduler) — when retention runs. Separate from the policy it invokes,
//!   because "what is safe to drop" and "when to ask" are independent decisions,
//!   and the second one being absent is what left the first unreachable.

pub mod cas;
pub mod consolidator;
pub mod fold;
pub mod history;
pub mod retention;
pub mod scheduler;

pub use consolidator::Consolidator;
pub use fold::{Delta, Folded};
pub use history::{Block, HistoryStore, NativeBlobState, SearchHit, SessionBytes};
pub use retention::{
    Confirmation, EnvagentAuthority, NativeSourceState, Resolved, RetentionAuthority,
    TerminalAuthority, WithheldReason,
};
pub use scheduler::{PassReport, RetentionScheduler};
