//! The duplex agent-session protocol: `sylph.agent.v3`.
//!
//! **This module is now framing only.** The vocabulary it used to hold — events,
//! capabilities, config, requests, forms — lives in `sylph-protocol`, because
//! `docs/session-protocol.md` § What changed makes an adapter a Rust trait rather than a
//! subprocess speaking NDJSON. What is left is the part that only exists because a
//! process boundary does.
//!
//! * [`wire`] is the format — one flat JSON object per line, in both directions.
//! * [`link`] is a live adapter process, with its handshake and correlation.
//!
//! Both go away in `docs/roadmap.md` § Implementation order: step 3 makes the server
//! call adapters in process, and step 4 gives the envagent the one framed boundary that
//! remains.

pub mod link;
#[cfg(test)]
mod adapter_tests;
pub mod wire;

pub use link::{Link, LinkConfig, LinkEvent};
