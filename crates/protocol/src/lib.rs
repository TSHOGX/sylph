//! Shared capture, trace, and operational control contracts. No runtime dependencies.

pub mod acquisition;
pub use acquisition::*;
pub mod adapter;
pub mod capabilities;
pub mod env;
pub mod identity;
pub mod link;
pub mod request;
pub mod semantic;
pub mod trace;

pub use adapter::{
    normalize_workdir, Adapter, AgentInfo, ControlStream, NativeSummary, RequestedNativeId,
    RunSpec, Session, SessionMessage, StoreFilter, TurnInput,
};
pub use capabilities::{
    Capability, CapabilityFacets, CommandKind, CommandSpec, ConfigChoice, ConfigKind, ConfigOption,
    FlagSchema, FlagSpec, SessionCapabilities, SubagentSpec,
};
pub use env::{
    decode_message, negotiate, AgentInventory, CapabilityResult, CapabilitySnapshot, Command,
    CommandResultMessage, DecodedMessage, Diagnostic, DiagnosticLevel, EnumeratedSession,
    EnvironmentCommand, EnvironmentHealth, FileOperation, Heartbeat, Hello, LaunchFlag,
    ProtocolVersion, RejectCode, Welcome, WireAttachment, WireError, WireMessage, WireTurnInput,
    WorkdirPolicy, MAX_FILE_READ_BYTES, MAX_FRAME_BYTES, PROTOCOL as ENV_PROTOCOL,
    QUICK_START_SENTINEL, VERSION_MAJOR as ENV_VERSION_MAJOR, VERSION_MINOR as ENV_VERSION_MINOR,
};
pub use identity::{
    AgentName, EnvironmentId, Fidelity, IdentityError, NativeId, SessionId, ID_DELIMITER,
};
pub use link::{AdapterError, Facet, LinkFacets, RefusalCode, UnavailableReason, VendorRefusal};
pub use request::{
    Choice, Field, FieldKind, Form, FormAnswer, NotesSpec, Request, RequestKind, RequestOption,
    Resolution, Risk, SelectMode,
};
pub use semantic::{
    CommandOutcome, CommandRequest, CommandRequestId, CommandResult, ControlReceipt,
    IdempotencyPolicy, ProducerId, SemanticError, SessionEdgeRelation, TurnLease,
};
