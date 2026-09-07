//! The event vocabulary and the adapter contract, as types.
//!
//! `docs/session-protocol.md` is the governing spec and this crate is that document
//! compiled. It is the foundation every other crate builds on: the adapters construct
//! these values, the server stores and serves them, and the envagent carries them across
//! a machine boundary.
//!
//! # What this crate is not
//!
//! **Not a transport.** `sylph.agent.v3` was a wire protocol between the server and a
//! subprocess speaking NDJSON on stdio. Its *vocabulary* was right and is kept almost
//! unchanged; its *transport* is what this rewrite removes. An adapter is a Rust trait
//! linked into the envagent, and NDJSON survives only on `sylph.env.v1`, where bytes
//! genuinely cross a machine boundary (`docs/environments.md`).
//!
//! So there is **no `v4`**. The vocabulary is versioned by the envagent wire protocol
//! it travels on, because that is now the only place a version can disagree with
//! anything. Nothing in this crate carries a protocol version string, and the framing —
//! `Inbound`, `Outbound`, `parse_inbound`, and the handshake constants — stays in
//! `backend/src/adapter/` until the envagent claims it in step 4. A reader looking for
//! where the seam falls: **a value that describes an agent or a session is here; a value
//! that describes how bytes are packed is not.**
//!
//! **Not a runtime.** No `tokio`, no `sqlx`, no `axum`. Every timestamp is an `i64` of
//! epoch milliseconds, which is what the journal stores and what the wire carries.
//!
//! # Two rules that hold everywhere in here
//!
//! **`serde`'s `rename_all` does not rename fields inside enum variants.** An internally
//! tagged enum needs `rename_all_fields` too, or a camelCase wire field inside a variant
//! parses as `None` while the code compiles and every non-variant field works. Every
//! such attribute is load-bearing; losing one is a silent wire break.
//!
//! **One fact has one derivation.** [`Event::stability`] is a field with exactly one
//! producer, [`CapabilityFacets::apply_to`] is the only merge, and
//! [`SessionId::encode`] is the only place the identity triple becomes a string. Each of
//! those is a place this repo has already paid for having two answers to one question.

pub mod adapter;
pub mod capabilities;
pub mod env;
pub mod event;
pub mod identity;
pub mod link;
pub mod request;
pub mod semantic;

// Re-exported flat, because a caller wants `sylph_protocol::Event` rather than
// `sylph_protocol::event::Event` — the modules are an organizing device for readers of
// this crate, not a namespace its consumers should have to learn. The modules stay
// public so a doc link can name one.
pub use adapter::{
    normalize_workdir, Adapter, AgentInfo, EventStream, NativeBlob, NativeRecord, NativeSummary,
    RequestedNativeId, RunSpec, Session, SessionMessage, StoreFilter, TurnInput,
};
pub use capabilities::{
    Capability, CapabilityFacets, CommandKind, CommandSpec, ConfigChoice, ConfigKind, ConfigOption,
    FlagSchema, FlagSpec, SessionCapabilities, SubagentSpec,
};
pub use env::{
    decode_message, negotiate, Ack, CapabilitySnapshot, Command, CommandResultMessage,
    DecodedMessage, Diagnostic, DiagnosticLevel, EnvironmentCommand, EnvironmentHealth, EventBatch,
    FileOperation, Heartbeat, Hello, Nack, ProtocolVersion, SourceState, Welcome, WireError,
    WireMessage, WireTurnInput, DEFAULT_MAX_BATCH_EVENTS, PROTOCOL as ENV_PROTOCOL,
    VERSION_MAJOR as ENV_VERSION_MAJOR, VERSION_MINOR as ENV_VERSION_MINOR,
};
pub use event::{Event, EventKind, InteriorState, Phase, Stability};
pub use identity::{
    AgentName, EnvironmentId, Fidelity, IdentityError, NativeId, SessionId, ID_DELIMITER,
};
pub use link::{AdapterError, Facet, LinkFacets, RefusalCode, UnavailableReason};
pub use request::{
    Choice, Field, FieldKind, Form, FormAnswer, NotesSpec, Request, RequestKind, RequestOption,
    Resolution, Risk, SelectMode,
};
pub use semantic::{
    CommandOutcome, CommandRequest, CommandRequestId, CommandResult, IdempotencyPolicy, ProducerId,
    ProducerKey, ProducerSeq, SemanticError, SequencedEvent, ServerSeq, SessionEdge,
    SessionEdgeRelation, SourceCursor, TurnInputFact,
};
