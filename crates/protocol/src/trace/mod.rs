//! Unified trace vocabulary: retained observations and typed, evidenced interpretations.
//!
//! The semantic authority is `docs/unified-trace-foundation.md`; capture and persistence
//! are governed by `docs/unified-trace-storage.md`. No runtime or vendor I/O belongs here.

pub mod capture;
pub mod claims;
pub mod control;
pub mod identity;
pub mod objects;
pub mod scope;
pub mod read;

pub use capture::*;
pub use claims::*;
pub use control::*;
pub use identity::*;
pub use objects::*;
pub use scope::*;
pub use read::*;

pub mod evidence_read;
pub use evidence_read::*;
