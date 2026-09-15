//! Durable evidence and operational facts for the unified trace.
//!
//! These independent writers implement `docs/unified-trace-storage.md`. Projection
//! workers consume committed admissions; neither a mapper nor a reader owns admission.

pub mod evidence;
pub mod exporter;
pub mod generation;
pub mod outbox;
pub mod worker;
pub mod publication;
pub mod upload;
pub mod gc;
