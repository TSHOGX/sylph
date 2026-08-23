//! The duplex agent-session protocol: `sylph.agent.v2`.
//!
//! `docs/session-protocol.md` is the governing spec. The short version of why
//! this module exists: the v1 runner contract writes a prompt to stdin and
//! closes it, and closing is what makes approvals, interrupt, mid-turn steering,
//! and live capability unreachable rather than merely unimplemented.
//!
//! * [`wire`] is the format — one flat JSON object per line, in both directions.
//! * [`link`] is a live adapter process, with its handshake and correlation.

pub mod link;
#[cfg(test)]
mod tests;
pub mod wire;

pub use link::{Link, LinkConfig, LinkEvent};
pub use wire::{AdapterRequest, Capability, SessionCapabilities};
