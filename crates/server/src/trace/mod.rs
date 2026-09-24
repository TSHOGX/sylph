//! Durable evidence and operational facts for the unified trace.
//!
//! These independent writers implement `docs/unified-trace-storage.md`. Projection
//! workers consume committed admissions; neither a mapper nor a reader owns admission.

pub mod cas;
pub(crate) mod cursor;
pub mod evidence;
pub mod exporter;
pub mod gc;
pub mod generation;
pub mod publication;
pub mod reading_store;
pub mod search;
pub mod service;
pub mod upload;
pub mod worker;

pub mod transfer;
pub mod structure;
