//! The semantic wire contract for an environment connection.
//!
//! This module contains serde-only values.  The byte framing lives in the
//! envagent runtime, which means protocol consumers that do not have a Tokio
//! runtime can still validate messages and negotiate versions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Workspace path sentinel for environment-local Quick Start.
pub const QUICK_START_SENTINEL: &str = "__sylph_quick_start__";

use crate::{
    AgentInfo, Capability, CapabilityFacets, CommandOutcome, CommandRequest, CommandRequestId,
    EnvironmentId, IdempotencyPolicy, LinkFacets, NativeId, ProducerId, Request,
    SessionCapabilities, SessionId,
};

pub const PROTOCOL: &str = "sylph.env.v1";
pub const VERSION_MAJOR: u16 = 1;
pub const VERSION_MINOR: u16 = 0;

/// Hard upper bound for one framed message, on both ends of the wire.
///
/// One constant rather than one per side, because the two sides used to each hold an
/// 8 MiB ceiling and nothing checked that they agreed. It is sized by the largest
/// payload a frame legitimately carries: a turn's attachments, which `docs/api/files.md`
/// caps at 10 MiB per turn and which travel base64-encoded (a third larger), and a
/// session file read, which the envagent caps at [`MAX_FILE_READ_BYTES`] before
/// encoding. Both fit with room; anything larger is a defect in a producer rather than
/// a frame that should have been allowed.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// The most bytes one `read` file operation returns before encoding.
///
/// The envagent refuses larger reads rather than truncating silently: a caller that
/// wants a prefix asks for one with `limit`, and a caller that asked for the whole file
/// must be told it did not get it.
pub const MAX_FILE_READ_BYTES: u64 = 32 * 1024 * 1024;

/// A protocol version offered by one side of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const CURRENT: Self = Self {
        major: VERSION_MAJOR,
        minor: VERSION_MINOR,
    };

    pub const fn compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}

/// One agent this build of the envagent knows, and whether that machine can run it.
///
/// **Known and unavailable is a state, and it is the state that names a fix.** An
/// inventory carrying only the installed agents cannot say whether `codex` is missing
/// because the CLI is not on `PATH` (install it) or because this envagent was never
/// built with it (upgrade the envagent), and a picker can only render "not here". So
/// every agent the envagent knows appears, with `available` and, when it is not, a
/// `reason` in words and the `version` it found if a CLI was present but refused.
///
/// `info` is the adapter's own [`AgentInfo`] — flags, controls, identity rules — and
/// is present exactly when `available` is true, since it comes from probing a CLI
/// that answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInventory {
    pub name: String,
    pub available: bool,
    /// The CLI's own version, when a probe reported one. Never synthesized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Why `available` is false, for a person. Absent when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<AgentInfo>,
}

impl AgentInventory {
    pub fn available(info: AgentInfo) -> Self {
        Self {
            name: info.name.clone(),
            available: true,
            version: info.version.clone(),
            reason: None,
            info: Some(info),
        }
    }

    pub fn unavailable(
        name: impl Into<String>,
        version: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            available: false,
            version,
            reason: Some(reason.into()),
            info: None,
        }
    }
}

/// The first message sent by an envagent after it connects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub protocol: String,
    pub versions: Vec<ProtocolVersion>,
    /// Absent only during the one-shot pairing exchange.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<EnvironmentId>,
    /// Stable id for one installation, distinct from the environment id.
    pub installation_id: String,
    pub producer_id: ProducerId,
    pub connection_epoch: u64,
    /// Every agent this envagent knows, available or not
    /// (`docs/environments.md` § Agent availability).
    #[serde(default)]
    pub agents: Vec<AgentInventory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<EnvironmentHealth>,
}

impl Hello {
    pub fn current(
        installation_id: impl Into<String>,
        producer_id: ProducerId,
        environment_id: Option<EnvironmentId>,
        connection_epoch: u64,
    ) -> Self {
        Self {
            protocol: PROTOCOL.to_string(),
            versions: vec![ProtocolVersion::CURRENT],
            environment_id,
            installation_id: installation_id.into(),
            producer_id,
            connection_epoch,
            agents: Vec::new(),
            health: None,
        }
    }
}

/// Server response to a hello.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub protocol: String,
    pub version: ProtocolVersion,
    pub environment_id: EnvironmentId,
    pub connection_epoch: u64,
    pub server_epoch: u64,
    pub heartbeat_interval_ms: u64,
}

impl Welcome {
    pub fn current(
        environment_id: EnvironmentId,
        connection_epoch: u64,
        server_epoch: u64,
    ) -> Self {
        Self {
            protocol: PROTOCOL.to_string(),
            version: ProtocolVersion::CURRENT,
            environment_id,
            connection_epoch,
            server_epoch,
            heartbeat_interval_ms: 15_000,
        }
    }
}

/// A server-to-envagent operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
// Values are bounded by the wire frame and queue limits; retain one owned serde envelope.
#[allow(clippy::large_enum_variant)]
pub enum EnvironmentCommand {
    /// Start a disposable prepared link on the envagent.  No canonical session
    /// row exists until a later `Run` promotes it with a first turn.
    Prepare {
        capture_authorization: crate::CaptureAuthorization,
        draft_id: String,
        agent: String,
        workdir: String,
        #[serde(default)]
        flags: BTreeMap<String, String>,
        #[serde(default)]
        inject_identity: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity_preamble: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
    },
    Run {
        capture_authorization: crate::CaptureAuthorization,
        session_id: Option<SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        draft_id: Option<String>,
        agent: String,
        workdir: String,
        #[serde(default)]
        flags: BTreeMap<String, String>,
        #[serde(default)]
        inject_identity: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity_preamble: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
        /// Optional first turn.  Supplying it lets an agent such as Codex,
        /// which assigns its native id only after accepting input, complete
        /// run+identity acquisition as one command.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial_input: Option<WireTurnInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial_turn_id: Option<String>,
    },
    Send {
        session_id: SessionId,
        turn_id: String,
        input: WireTurnInput,
    },
    Interrupt {
        session_id: SessionId,
        /// Optional turn identity guard.  A stop request carries this so a
        /// delayed interrupt cannot terminate a newer turn that began after
        /// the original one was settled.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_turn_id: Option<String>,
    },
    Steer {
        session_id: SessionId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_turn_id: Option<String>,
    },
    QueueSteering {
        session_id: SessionId,
        text: String,
    },
    Queue {
        session_id: SessionId,
        text: String,
    },
    SetConfig {
        session_id: SessionId,
        id: String,
        value: serde_json::Value,
    },
    Resolve {
        session_id: SessionId,
        resolution: serde_json::Value,
    },
    Watch {
        capture_authorization: crate::CaptureAuthorization,
        session_id: SessionId,
        agent: String,
        native_id: NativeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    Adopt {
        capture_authorization: crate::CaptureAuthorization,
        session_id: SessionId,
        agent: String,
        native_id: NativeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    /// Reacquire native lexical sources through the shared durable capture registry.
    /// The server reprojects from retained evidence without an adapter-side semantic API.
    Reread {
        capture_authorization: crate::CaptureAuthorization,
        session_id: SessionId,
        agent: String,
        native_id: NativeId,
    },
    /// Contained filesystem access under the server-recorded workdir.
    Files {
        session_id: SessionId,
        /// The server's recorded root, independent of any process-local hosted link.
        workdir: String,
        operation: FileOperation,
    },
    DraftCapabilities {
        draft_id: String,
    },
    DraftSetConfig {
        draft_id: String,
        id: String,
        value: serde_json::Value,
    },
    DraftFiles {
        draft_id: String,
        operation: FileOperation,
    },
    DraftClose {
        draft_id: String,
    },
    Health,
    Capabilities {
        session_id: SessionId,
    },
    Enumerate {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<String>,
    },
    /// Close a held process or stop following its native sources.
    Close {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
    },
    /// The directories a client may start browsing from on this machine, for naming
    /// a session's `workdir`.
    ///
    /// Answered by the envagent because the roots are its own `browse_roots`
    /// (`docs/configuration.md` § `browse_roots` bounds where agents run): only the
    /// machine holding a path can canonicalize it, so the server never resolves one.
    BrowseRoots,
    /// One directory level of subdirectories under a browsable root.
    ///
    /// `path` absent means the first root, so a client's first request needs no prior
    /// knowledge of them.
    BrowseTree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
}

/// One attachment a client posted with a turn, as it crosses the wire.
///
/// **Bytes, not a path.** The server used to stage attachments on its own disk and
/// hand the adapter an absolute path, which worked exactly while the adapter ran in
/// the server's process. An adapter runs in the envagent now, on whatever machine the
/// session lives on, so a server-side path names a file it cannot open. The envagent
/// stages the bytes under its own root and hands the adapter a local path.
///
/// `data` is base64 with no `data:` prefix — the same encoding a browser posted it
/// with, so nothing decodes and re-encodes it on the way through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireAttachment {
    pub name: String,
    #[serde(rename = "type")]
    pub mime_type: String,
    pub data: String,
}

/// One native session in an environment's store, as `enumerate` answers it.
///
/// A [`crate::NativeSummary`] plus the one fact only the machine holding the store can add:
/// whether its recorded directory is one this environment permits an agent to run in.
/// The server used to classify that itself against its own roots, which was a check
/// against a filesystem it could not see the moment the store lived elsewhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumeratedSession {
    #[serde(flatten)]
    pub summary: crate::NativeSummary,
    pub workdir_policy: WorkdirPolicy,
}

/// What the envagent found when it checked a recorded working directory.
///
/// Each value is one of the adopt refusals, decided before the request rather than
/// after — the same classification, so a picker cannot disagree with the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkdirPolicy {
    /// Exists, and resolves inside a browse root. Adopting will be accepted.
    Contained,
    /// Exists, and resolves outside every root. Adopting is refused; widening
    /// `browse_roots` on that machine is the fix.
    Outside,
    /// The recorded directory is gone. No configuration brings it back.
    Missing,
    /// The store recorded no directory at all. Nothing can make it adoptable, because
    /// guessing one would decide where arbitrary code runs.
    Unrecorded,
}

/// Why an envagent refused a command.
///
/// **A code the server branches on, never a message it matches.** The server used to
/// classify a refusal by searching its text for `"outside"` or `"No such file"`, which
/// is the shape `AGENTS.md` names as the expensive defect: a reworded message turned a
/// `403` into a `503` with nothing failing. Every rejection an envagent produces names
/// one of these, and the server maps each to exactly one HTTP error.
///
/// Carried as a string on the wire so an older server can still read a code a newer
/// envagent adds; [`RejectCode::parse`] answers `None` for one it does not know, and
/// the server reports that as a link failure carrying the original text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RejectCode {
    /// This environment does not know an agent by that name at all.
    AgentUnknown,
    /// The agent is known and its CLI is not on this machine's `PATH`. An install, not a
    /// retry.
    AgentUnavailable,
    /// The CLI is present and would not say what it is, or would not start.
    AgentStartFailed,
    /// The requested working directory does not exist.
    WorkdirMissing,
    /// The requested working directory exists and is not a directory.
    WorkdirNotADirectory,
    /// The requested working directory resolves outside every browse root.
    WorkdirOutsideRoots,
    /// No live session is hosted here under that id. Resuming is the remedy.
    SessionNotHosted,
    /// No prepared draft is held here under that handle.
    DraftNotFound,
    /// The named turn is no longer the active one, so a guarded interrupt did nothing.
    TurnMoved,
    /// The agent's own store holds no session by that id.
    NativeSessionNotFound,
    /// The store holds the session and recorded no working directory for it.
    NativeSessionNotResumable,
    /// The draft handle is malformed.
    InvalidDraftId,
    /// A draft could not be prepared.
    PrepareFailed,
    /// A run could not be started or resumed.
    RunFailed,
    /// An attachment or turn input could not be accepted.
    AttachmentInvalid,
    /// A turn input could not be sent.
    SendFailed,
    /// A capability snapshot could not be produced.
    CapabilitiesFailed,
    /// A config change could not be applied.
    SetConfigFailed,
    /// A native source could not be watched.
    WatchFailed,
    /// A native session enumeration failed.
    EnumerateFailed,
    /// A file command failed.
    FilesFailed,
    /// The option is not one this session's surface offers.
    ConfigOptionUnknown,
    /// The option exists and the value is not one it enumerated.
    ConfigValueNotOffered,
    /// The adapter refused the option as one the agent does not have.
    UnsupportedField,
    /// The adapter refused this value, and the control stays usable.
    InvalidValue,
    /// A file path was malformed: absolute, or naming no final component.
    InvalidPath,
    /// A file path resolves outside the directory it must stay within.
    PolicyViolation,
    FileNotFound,
    FileExists,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    /// A file read was refused because the file exceeds [`MAX_FILE_READ_BYTES`].
    FileTooLarge,
    /// The adapter or the machine failed in a way none of the above describes; the
    /// message is the diagnosis.
    Internal,
}

impl RejectCode {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentUnknown => "AGENT_UNKNOWN",
            Self::AgentUnavailable => "AGENT_UNAVAILABLE",
            Self::AgentStartFailed => "AGENT_START_FAILED",
            Self::WorkdirMissing => "WORKDIR_MISSING",
            Self::WorkdirNotADirectory => "WORKDIR_NOT_A_DIRECTORY",
            Self::WorkdirOutsideRoots => "WORKDIR_OUTSIDE_ROOTS",
            Self::SessionNotHosted => "SESSION_NOT_HOSTED",
            Self::DraftNotFound => "DRAFT_NOT_FOUND",
            Self::TurnMoved => "TURN_MOVED",
            Self::NativeSessionNotFound => "NATIVE_SESSION_NOT_FOUND",
            Self::NativeSessionNotResumable => "NATIVE_SESSION_NOT_RESUMABLE",
            Self::InvalidDraftId => "INVALID_DRAFT_ID",
            Self::PrepareFailed => "PREPARE_FAILED",
            Self::RunFailed => "RUN_FAILED",
            Self::AttachmentInvalid => "ATTACHMENT_INVALID",
            Self::SendFailed => "SEND_FAILED",
            Self::CapabilitiesFailed => "CAPABILITIES_FAILED",
            Self::SetConfigFailed => "SET_CONFIG_FAILED",
            Self::WatchFailed => "WATCH_FAILED",
            Self::EnumerateFailed => "ENUMERATE_FAILED",
            Self::FilesFailed => "FILES_FAILED",
            Self::ConfigOptionUnknown => "CONFIG_OPTION_UNKNOWN",
            Self::ConfigValueNotOffered => "CONFIG_VALUE_NOT_OFFERED",
            Self::UnsupportedField => "UNSUPPORTED_FIELD",
            Self::InvalidValue => "INVALID_VALUE",
            Self::InvalidPath => "INVALID_PATH",
            Self::PolicyViolation => "POLICY_VIOLATION",
            Self::FileNotFound => "FILE_NOT_FOUND",
            Self::FileExists => "FILE_EXISTS",
            Self::NotADirectory => "NOT_A_DIRECTORY",
            Self::IsADirectory => "IS_A_DIRECTORY",
            Self::DirectoryNotEmpty => "DIRECTORY_NOT_EMPTY",
            Self::FileTooLarge => "FILE_TOO_LARGE",
            Self::Internal => "INTERNAL",
        }
    }

    /// The code a wire string names, or `None` for one this build does not know.
    pub fn parse(code: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(code.to_string())).ok()
    }
}

/// A command envelope.  The request id and retry policy are mandatory even
/// for operations that are otherwise read-only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub request_id: CommandRequestId,
    pub idempotency: IdempotencyPolicy,
    pub command: EnvironmentCommand,
}

impl Command {
    pub fn new(
        request_id: CommandRequestId,
        idempotency: IdempotencyPolicy,
        command: EnvironmentCommand,
    ) -> Self {
        Self {
            request_id,
            idempotency,
            command,
        }
    }

    pub fn request(&self) -> CommandRequest {
        CommandRequest {
            request_id: self.request_id.clone(),
            idempotency: self.idempotency,
        }
    }
}

/// A command result.  Unknown is intentionally not converted to an error:
/// it means the command may have reached a vendor process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResultMessage {
    pub request_id: CommandRequestId,
    pub outcome: CommandOutcome,
    pub connection_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<IdempotencyPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heartbeat {
    pub connection_epoch: u64,
    pub sent_at_ms: i64,
}

/// A capability update is a partial snapshot.  The server merges it rather
/// than replacing facets that were not mentioned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySnapshot {
    pub session_id: SessionId,
    pub facets: CapabilityFacets,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkFacets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<SessionCapabilities>,
    pub connection_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchFlag {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityResult {
    pub capabilities: SessionCapabilities,
    pub supports: Vec<Capability>,
    pub link: LinkFacets,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted_launch_flag: Option<LaunchFlag>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentHealth {
    pub connected: bool,
    #[serde(default)]
    pub agents: Vec<AgentInventory>,
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub details: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireTurnInput {
    #[serde(default)]
    pub text: String,
    /// The bytes a client posted, for the envagent to stage where its adapter can read
    /// them. See [`WireAttachment`] for why these are not paths.
    #[serde(default)]
    pub attachments: Vec<WireAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: BTreeMap<String, String>,
    #[serde(default)]
    pub config: BTreeMap<String, serde_json::Value>,
}

/// File operations are path names relative to the session workdir.  The
/// envagent resolves and contains them before touching its filesystem.
///
/// File bytes travel **base64-encoded in a string**, in both directions. A `Vec<u8>`
/// serializes to a JSON array of integers, up to four bytes of text per byte of file,
/// and a one-megabyte preview became a four-megabyte frame — half the frame ceiling
/// for a file the pane shows the top of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum FileOperation {
    List {
        relative: String,
    },
    /// Read a file, or its first `limit` bytes.
    ///
    /// The answer carries the file's whole size beside the base64 bytes it returned,
    /// so a caller that asked for a prefix knows it got one. With no `limit` the
    /// whole file is returned, up to [`MAX_FILE_READ_BYTES`].
    Read {
        relative: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
    },
    Write {
        relative: String,
        /// Base64, no `data:` prefix.
        data: String,
        #[serde(default)]
        overwrite: bool,
    },
    Stat {
        relative: String,
    },
    Mkdir {
        relative: String,
    },
    Remove {
        relative: String,
        #[serde(default)]
        recursive: bool,
    },
}

/// The message set accepted on either side.  Direction is checked by the
/// connection state machine; keeping one enum makes fixture transcripts easy
/// to replay.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WireMessage {
    SessionControlState {
        session_id: SessionId,
        lease: Option<String>,
        native_turn_id: Option<String>,
        active: bool,
        connection_epoch: u64,
    },

    CaptureUpload {
        request_id: String,
        operation: crate::CaptureUpload,
        connection_epoch: u64,
    },
    CaptureReply {
        request_id: String,
        outcome: crate::CaptureReply,
        connection_epoch: u64,
    },

    Hello {
        protocol: String,
        versions: Vec<ProtocolVersion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment_id: Option<EnvironmentId>,
        installation_id: String,
        producer_id: ProducerId,
        connection_epoch: u64,
        #[serde(default)]
        agents: Vec<AgentInventory>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        health: Option<EnvironmentHealth>,
    },
    Welcome {
        protocol: String,
        version: ProtocolVersion,
        environment_id: EnvironmentId,
        connection_epoch: u64,
        server_epoch: u64,
        heartbeat_interval_ms: u64,
    },
    Command {
        request_id: CommandRequestId,
        idempotency: IdempotencyPolicy,
        command: EnvironmentCommand,
    },
    CommandResult {
        request_id: CommandRequestId,
        outcome: CommandOutcome,
        connection_epoch: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idempotency: Option<IdempotencyPolicy>,
    },

    Heartbeat {
        connection_epoch: u64,
        sent_at_ms: i64,
    },
    /// The envagent's agent inventory changed while connected.
    ///
    /// `docs/environments.md` § Agent availability: which agents a machine can run is
    /// discovered in `hello` and **re-discovered over the life of the connection**, so
    /// upgrading a vendor CLI or installing a new one takes effect without a re-pair or
    /// a server restart. Not sequenced through the spool, like a heartbeat: it is current
    /// state about the machine rather than a fact about a session's history, and a
    /// reconnect re-reports it in `hello` anyway.
    Agents {
        connection_epoch: u64,
        agents: Vec<AgentInventory>,
    },
    CapabilitySnapshot {
        session_id: SessionId,
        facets: CapabilityFacets,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link: Option<LinkFacets>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capabilities: Option<SessionCapabilities>,
        connection_epoch: u64,
    },

    Request {
        session_id: SessionId,
        request: Request,
        connection_epoch: u64,
    },

    Diagnostic {
        level: DiagnosticLevel,
        code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
        #[serde(default)]
        details: BTreeMap<String, serde_json::Value>,
        connection_epoch: u64,
    },
}

impl WireMessage {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SessionControlState { .. } => "session_control_state",
            Self::CaptureUpload { .. } => "capture_upload",
            Self::CaptureReply { .. } => "capture_reply",
            Self::Hello { .. } => "hello",
            Self::Welcome { .. } => "welcome",
            Self::Command { .. } => "command",
            Self::CommandResult { .. } => "command_result",
            Self::Heartbeat { .. } => "heartbeat",
            Self::Agents { .. } => "agents",
            Self::CapabilitySnapshot { .. } => "capability_snapshot",
            Self::Request { .. } => "request",
            Self::Diagnostic { .. } => "diagnostic",
        }
    }

    pub fn from_hello(hello: Hello) -> Self {
        Self::Hello {
            protocol: hello.protocol,
            versions: hello.versions,
            environment_id: hello.environment_id,
            installation_id: hello.installation_id,
            producer_id: hello.producer_id,
            connection_epoch: hello.connection_epoch,
            agents: hello.agents,
            health: hello.health,
        }
    }
}

/// Decode a frame while preserving an unknown message kind for forward
/// compatibility.  Known kinds still fail loudly when their payload is
/// malformed; silently accepting a truncated event would create a gap.
#[derive(Debug, Clone, PartialEq)]
// `Known` is the hot path and deliberately stays inline: boxing every decoded
// message would add an allocation to each event batch. Unknown extensions are
// rare and already own a dynamically-sized `Value`, so the size difference is
// an intentional transport trade-off rather than an accidental variant bloat.
#[allow(clippy::large_enum_variant)]
pub enum DecodedMessage {
    Known(WireMessage),
    Unknown {
        kind: String,
        raw: serde_json::Value,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("invalid JSON wire message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("wire message has no string type")]
    MissingType,
    #[error("wire message type is empty")]
    EmptyType,
    #[error("known wire message '{kind}' is malformed: {message}")]
    MalformedKnown { kind: String, message: String },
    #[error("no compatible protocol version was offered")]
    NoCompatibleVersion,
}

pub fn decode_message(bytes: &[u8]) -> Result<DecodedMessage, WireError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let object = value.as_object().ok_or(WireError::MissingType)?;
    let kind = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or(WireError::MissingType)?;
    if kind.is_empty() {
        return Err(WireError::EmptyType);
    }
    match serde_json::from_value::<WireMessage>(value.clone()) {
        Ok(message) => Ok(DecodedMessage::Known(message)),
        Err(error) if known_kind(kind) => Err(WireError::MalformedKnown {
            kind: kind.to_string(),
            message: error.to_string(),
        }),
        Err(_) => Ok(DecodedMessage::Unknown {
            kind: kind.to_string(),
            raw: value,
        }),
    }
}

fn known_kind(kind: &str) -> bool {
    matches!(
        kind,
        "capture_upload"
            | "capture_reply"
            | "session_control_state"
            | "hello"
            | "welcome"
            | "command"
            | "command_result"
            | "heartbeat"
            | "agents"
            | "capability_snapshot"
            | "request"
            | "diagnostic"
    )
}

pub fn negotiate(offers: &[ProtocolVersion]) -> Result<ProtocolVersion, WireError> {
    offers
        .iter()
        .copied()
        .find(|candidate| candidate.compatible_with(ProtocolVersion::CURRENT))
        .map(|_candidate| {
            // Minor versions are additive.  The lower value is the feature
            // set both sides can safely assume.
            ProtocolVersion {
                major: VERSION_MAJOR,
                // v1.0 is the only published minor today; selecting the
                // current value prevents a future peer from advertising an
                // extension this build cannot understand.
                minor: VERSION_MINOR,
            }
        })
        .ok_or(WireError::NoCompatibleVersion)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hello_and_welcome_round_trip_with_explicit_epochs() {
        let hello = Hello::current(
            "install-1",
            ProducerId::parse("producer-1").unwrap(),
            Some(EnvironmentId::parse("env7fk2").unwrap()),
            4,
        );
        let message = WireMessage::from_hello(hello.clone());
        let bytes = serde_json::to_vec(&message).unwrap();
        let DecodedMessage::Known(WireMessage::Hello {
            connection_epoch,
            producer_id,
            ..
        }) = decode_message(&bytes).unwrap()
        else {
            panic!("hello must decode");
        };
        assert_eq!(connection_epoch, 4);
        assert_eq!(producer_id.as_str(), "producer-1");

        let welcome = Welcome::current(EnvironmentId::parse("env7fk2").unwrap(), 4, 8);
        assert_eq!(negotiate(&hello.versions).unwrap(), welcome.version);
        assert_eq!(
            negotiate(&[ProtocolVersion { major: 1, minor: 0 }])
                .unwrap()
                .minor,
            0
        );
    }

    #[test]
    fn unknown_message_types_are_forward_compatible() {
        let decoded = decode_message(br#"{"type":"future","value":{"x":1}}"#).unwrap();
        assert!(matches!(
            decoded,
            DecodedMessage::Unknown { kind, .. } if kind == "future"
        ));
    }

    #[test]
    fn known_wire_messages_reject_unrecognized_fields() {
        assert!(matches!(
            decode_message(
                br#"{"type":"heartbeat","connectionEpoch":1,"sentAtMs":2,"producerSeq":3}"#
            ),
            Err(WireError::MalformedKnown { .. })
        ));
    }

    #[test]
    fn malformed_known_payload_is_rejected_before_it_can_create_a_gap() {
        let decoded = decode_message(br#"{"type":"capture_upload","operation":"not-an-object"}"#);
        assert!(
            matches!(decoded, Err(WireError::MalformedKnown { kind, .. }) if kind == "capture_upload")
        );
    }

    #[test]
    fn command_policy_survives_the_flat_wire_shape() {
        let command = WireMessage::Command {
            request_id: CommandRequestId::parse("req-1").unwrap(),
            idempotency: IdempotencyPolicy::AtMostOnce,
            command: EnvironmentCommand::Health,
        };
        let value = serde_json::to_value(command).unwrap();
        assert_eq!(value["type"], "command");
        assert_eq!(value["requestId"], "req-1");
        assert_eq!(value["idempotency"], "at_most_once");
        assert_eq!(value["command"]["kind"], "health");
    }

    #[test]
    fn generated_schema_mentions_every_wire_message_kind() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/sylph.env.v1.schema.json"))
                .expect("the checked-in env wire schema must be valid JSON");
        let refs: std::collections::BTreeSet<_> = schema["oneOf"]
            .as_array()
            .expect("schema oneOf")
            .iter()
            .filter_map(|entry| entry["$ref"].as_str())
            .map(str::to_owned)
            .collect();
        let expected = [
            "captureUpload",
            "captureReply",
            "sessionControlState",
            "hello",
            "welcome",
            "command",
            "commandResult",
            "heartbeat",
            "agents",
            "capabilitySnapshot",
            "request",
            "diagnostic",
        ]
        .into_iter()
        .map(|name| format!("#/$defs/{name}"))
        .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(refs, expected);
        let command = &schema["$defs"]["command"]["properties"]["command"]["oneOf"];
        let command_refs: std::collections::BTreeSet<_> = command
            .as_array()
            .expect("command oneOf")
            .iter()
            .filter_map(|entry| entry["$ref"].as_str())
            .collect();
        assert!(
            command_refs.contains("#/$defs/commandInterrupt"),
            "interrupt must be described by the checked-in command schema"
        );
        assert!(
            schema["$defs"]["commandInterrupt"]["properties"]["expectedTurnId"].is_object(),
            "turn guard must be present in the protocol artifact"
        );
        assert!(schema["$defs"]["fileOperation"]["oneOf"].is_array());
        for browse in ["#/$defs/commandBrowseRoots", "#/$defs/commandBrowseTree"] {
            assert!(
                command_refs.contains(browse),
                "{browse} must be described by the checked-in command schema"
            );
        }
        // Attachments are bytes on the wire, not server-side paths, and the schema is
        // where a reader learns that.
        assert_eq!(
            schema["$defs"]["wireTurnInput"]["properties"]["attachments"]["items"]["$ref"],
            "#/$defs/wireAttachment"
        );
        assert!(
            schema["$defs"]["wireAttachment"]["properties"]["data"].is_object(),
            "an attachment carries its bytes, not a server-side path"
        );
    }

    /// The code is the contract; the message is for a person.
    ///
    /// Round-tripped through the wire spelling because that is what the server parses:
    /// a variant whose `as_str` and serde spelling disagreed would be emitted by the
    /// envagent under one name and looked up by the server under another, and the
    /// lookup would answer `None` — the "unknown code" branch, which reads as a link
    /// failure rather than the refusal it was.
    #[test]
    fn every_reject_code_parses_from_its_own_wire_spelling() {
        let codes = [
            RejectCode::AgentUnknown,
            RejectCode::AgentUnavailable,
            RejectCode::AgentStartFailed,
            RejectCode::WorkdirMissing,
            RejectCode::WorkdirNotADirectory,
            RejectCode::WorkdirOutsideRoots,
            RejectCode::SessionNotHosted,
            RejectCode::DraftNotFound,
            RejectCode::TurnMoved,
            RejectCode::NativeSessionNotFound,
            RejectCode::NativeSessionNotResumable,
            RejectCode::InvalidDraftId,
            RejectCode::PrepareFailed,
            RejectCode::RunFailed,
            RejectCode::AttachmentInvalid,
            RejectCode::SendFailed,
            RejectCode::CapabilitiesFailed,
            RejectCode::SetConfigFailed,
            RejectCode::WatchFailed,
            RejectCode::EnumerateFailed,
            RejectCode::FilesFailed,
            RejectCode::ConfigOptionUnknown,
            RejectCode::ConfigValueNotOffered,
            RejectCode::UnsupportedField,
            RejectCode::InvalidValue,
            RejectCode::InvalidPath,
            RejectCode::PolicyViolation,
            RejectCode::FileNotFound,
            RejectCode::FileExists,
            RejectCode::NotADirectory,
            RejectCode::IsADirectory,
            RejectCode::DirectoryNotEmpty,
            RejectCode::FileTooLarge,
            RejectCode::Internal,
        ];
        for code in codes {
            assert_eq!(RejectCode::parse(code.as_str()), Some(code), "{code:?}");
            assert_eq!(
                serde_json::to_value(code).unwrap(),
                serde_json::Value::String(code.as_str().to_string()),
                "{code:?}"
            );
        }
        assert_eq!(RejectCode::parse("NOT_A_CODE"), None);
        // The adapter's own refusal codes share a spelling with these two, so a
        // refusal crosses the wire without translation.
        assert_eq!(
            serde_json::to_value(crate::RefusalCode::UnsupportedField).unwrap(),
            serde_json::Value::String(RejectCode::UnsupportedField.as_str().into())
        );
        assert_eq!(
            serde_json::to_value(crate::RefusalCode::InvalidValue).unwrap(),
            serde_json::Value::String(RejectCode::InvalidValue.as_str().into())
        );
    }
}
