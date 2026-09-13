//! The Sylph environment agent.
//!
//! An envagent owns the machine-local adapter processes and native stores.  It
//! dials the server over `sylph.env.v1`; the server never reaches into this
//! filesystem.  The crate is split into small pieces so the correctness-critical
//! parts (framing, the ack-gated spool, and path policy) can be tested without a
//! vendor CLI or a network.

pub mod adapters;
pub mod attachments;
pub mod client;
pub mod credentials;
pub mod fixture;
pub mod health;
mod native_transfer;
pub mod path;
pub mod runtime;
pub mod spool;
pub mod tls;
pub mod transport;

pub use client::{
    connect as connect_to_server, run_runtime, run_runtime_reconnecting, run_runtime_stream,
    run_runtime_stream_with_adapters, ClientConnection,
};
pub use credentials::{Credential, CredentialStore};
pub use fixture::{FaultPlan, FaultPoint, FixtureSink};
pub use health::{AgentHealth, EnvironmentHealth, HealthReporter};
pub use path::{ContainmentError, PathPolicy};
pub use runtime::{
    ChannelEventSink, EnvAgent, EnvAgentConfig, EventSink, HostedSession, RecordingSink,
};
pub use spool::{Spool, SpoolConfig, SpoolError, SpoolEvent};
pub use transport::{Backoff, ConnectionEpoch, ConnectionSupervisor, FrameError, FramedConnection};
