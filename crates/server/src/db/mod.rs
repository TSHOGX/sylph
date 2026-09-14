//! The hot tier: unconsolidated turns, and the sessions that own them.
//!
//! Named `journal` rather than `live` because that is what the file *is* — a
//! write-ahead record of turns that have not settled yet, bounded by
//! consolidation — rather than *when* it is read. `live` also collided with the
//! product invariant that this tier is authoritative only while a turn is active:
//! the old filename claimed exactly the property the invariant limits.

pub mod controls;
pub mod env;
/// Writing an adapter's parse of a vendor's native store into this journal.
///
/// Beside the store rather than in `api::sessions` because it is a *storage* decision:
/// the one question an adapter cannot answer is which turn row an event belongs to,
/// and `turns` is Sylph's schema rather than any vendor's.
pub mod ingest;
/// Deriving a session's label from what its transcript actually says.
///
/// Beside the store rather than inside it because these are rules about
/// transcripts, not about this table — see the module's own header for why they
/// outlived the separate archive they arrived with.
pub mod labels;
pub mod store;
/// Batching the journal's hot path. Separate from [`store`] because *when* a write
/// commits is a different decision from *what* it writes, and only the first one
/// depends on whether a reconnect can replay the event from anywhere else.
pub mod writer;

pub use env::{EnvIngestOutcome, EnvIngestResult, EnvReceipt, EnvironmentRow};
pub use ingest::{IngestReport, NativeIngest};
pub use store::JournalStore;
pub use writer::JournalWriter;
