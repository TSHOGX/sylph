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

mod search;
pub mod timeline;

/// All projections derived while accepting a canonical event share its transaction.
pub(crate) async fn project_event(
    conn: &mut sqlx::SqliteConnection,
    event: &crate::models::Event,
) -> crate::error::Result<()> {
    // The first root input owns the prompt. Additional inputs in the same turn
    // keep their own event identity rather than replacing the original request.
    // This must precede node projection and share the producer receipt transaction.
    if event.kind == "user" && event.span.is_none() && !event.text.is_empty() {
        sqlx::query("UPDATE turns SET request_text=? WHERE id=? AND session_env=? AND session_agent=? AND session_native=? AND request_text=''")
            .bind(&event.text).bind(&event.turn_id)
            .bind(event.session_id.environment.as_str()).bind(event.session_id.agent.as_str())
            .bind(event.session_id.native_id.as_str()).execute(&mut *conn).await?;
    }
    JournalStore::project_event_title(conn, event).await?;
    timeline::project(conn, event).await
}
