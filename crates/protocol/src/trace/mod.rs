//! Unified trace vocabulary: retained observations and typed, evidenced interpretations.
//!
//! The semantic authority is `docs/unified-trace-foundation.md`; capture and persistence
//! are governed by `docs/unified-trace-storage.md`. No runtime or vendor I/O belongs here.

pub mod capture;
pub mod claims;
pub mod identity;
pub mod objects;

pub use capture::*;
pub use claims::*;
pub use identity::*;
pub use objects::*;
