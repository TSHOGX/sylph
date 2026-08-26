//! The hot tier: unconsolidated turns, and the sessions that own them.
//!
//! Named `journal` rather than `live` because that is what the file *is* — a
//! write-ahead record of turns that have not settled yet, bounded by
//! consolidation — rather than *when* it is read. `live` also collided with the
//! product invariant that this tier is authoritative only while a turn is active:
//! the old filename claimed exactly the property the invariant limits.

pub mod store;
/// Batching the journal's hot path. Separate from [`store`] because *when* a write
/// commits is a different decision from *what* it writes, and only the first one
/// depends on whether a reconnect can replay the event from anywhere else.
pub mod writer;

pub use store::JournalStore;
pub use writer::JournalWriter;
