//! Pure semantic mapping and deterministic trace reduction over retained evidence.
//!
//! No filesystem, process, network, or current-clock dependency belongs in this crate.
//! Callers supply a fixed evidence cut and archive context and persist returned state.

pub mod mapping;
pub mod reducer;
pub mod reading;
