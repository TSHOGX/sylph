//! Durable, replayable history.
//!
//! `docs/storage.md` is the governing spec. Three tiers, because there are three
//! workloads with opposite optimal shapes:
//!
//! * [`fold`] — a block's deltas as offsets and timings, so lossless replay costs
//!   ~8 bytes per delta rather than a row.
//! * [`cas`] — content-addressed blobs, so a file an agent reads in twenty turns
//!   is stored once.
//! * [`history`] — the read-mostly store the journal consolidates into.
//! * [`retention`] — watermark-driven pruning, never time-driven.

pub mod cas;
pub mod consolidator;
pub mod fold;
pub mod history;
pub mod retention;

pub use fold::{Delta, Folded};
pub use consolidator::Consolidator;
pub use history::{Block, HistoryStore};
pub use retention::{RetentionAuthority, TerminalAuthority, TraceDbAuthority};
