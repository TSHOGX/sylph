/**
 * API client for the Sylph Web backend.
 *
 * All requests go through the Next.js rewrite (`/api/v1/*` -> backend on 4317).
 * Auth is cookie-based: `GET /api/v1/auth/token` sets `sylph_session`, which the
 * browser then sends on every subsequent request. No explicit Authorization
 * header is needed.
 *
 * The backend serializes every model with `#[serde(rename_all = "camelCase")]`,
 * so wire fields are `createdAtMs`, `identityMode`, `requestText` and so on.
 * These interfaces mirror the wire shape exactly; do not "tidy" them into
 * snake_case, because that silently yields `undefined` at runtime rather than a
 * type error.
 */

const API_BASE = "/api/v1";

export type SessionOrigin = "sylph" | "native";
export type SessionStatus = "idle" | "running";

export interface Session {
  id: string;
  origin: SessionOrigin;
  /** Identity policy: `sylph` injects SOUL.md, `native` preserves the agent. */
  identityMode: string;
  /** Lifecycle policy: governs reset and compaction eligibility. */
  lifecycleMode: string;
  agent: string;
  /** Agent-native id returned by the runner, when a turn has started. */
  nativeSessionId?: string;
  workdir: string;
  /** Nullable: the backend stores title as an optional column. */
  title: string | null;
  /**
   * TraceDB id this session was imported from, when it was.
   *
   * Present only for imported native sessions. The archive browser uses it to
   * recognize that an archived row and a live session are one conversation, and
   * list it once rather than twice.
   */
  traceSessionId?: string;
  flagsJson: string;
  status: SessionStatus;
  createdAtMs: number;
  updatedAtMs: number;
}

export type TurnStatus =
  | "queued"
  | "running"
  | "stopping"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted";

export interface Turn {
  id: string;
  sessionId: string;
  incarnationId: string | null;
  status: TurnStatus;
  requestText: string;
  commandInvocationId: string | null;
  idempotencyKey: string | null;
  errorCode: string | null;
  errorMessage: string | null;
  runnerExitCode: number | null;
  createdAtMs: number;
  startedAtMs: number | null;
  completedAtMs: number | null;
  stoppedAtMs: number | null;
}

/** A normalized journal/stream event. Mirrors the backend `StreamEvent`. */
export interface StreamEvent {
  seq: number;
  kind: string;
  phase?: string;
  streamId?: string;
  /**
   * The delegated trajectory this event happened in; absent is the root turn.
   *
   * `docs/session-protocol.md` § Delegated trajectories. A `subagent` event's
   * `streamId` *is* a trajectory id, so a nested delegation is one whose `span`
   * names another — which is how arbitrary depth and a dynamic workflow's
   * parallel phases are carried by a single field.
   */
  span?: string;
  name?: string;
  text?: string;
  data?: unknown;
  isError?: boolean;
  stability: string;
  source: string;
  createdAtMs: number;
}

/** One turn plus its durable events, as returned by the history endpoint. */
export interface TurnWithEvents {
  turn: Turn;
  events: StreamEvent[];
}

/** A flag declared by an agent's handshake. */
export interface AgentFlag {
  choices: string[];
  description?: string;
}

/**
 * Whether a command is something to ask the agent, or something the session *is*.
 *
 * `prompt` runs as a turn; `control` opens the picker for the config option it
 * names. Absent means `prompt`, so an agent that never says behaves as before.
 *
 * This is what stops `/model` coming back as a paragraph: sent as prompt text to a
 * headless CLI, "which model?" can only be answered in prose, because there is no
 * picker on that side of the wire.
 */
export type CommandKind = "prompt" | "control";

/**
 * A slash command the *session's own agent* reports.
 *
 * Distinct from `AgentCommand` in where it comes from: this list is read from a
 * live agent that has loaded the project, so it holds the real skills, project
 * commands, and plugins — dozens on a real checkout. `AgentCommand` is what an
 * agent declares about itself before any session exists, and is usually empty.
 */
export interface SessionCommand {
  name: string;
  description?: string;
  /** What follows the command, e.g. `[target]`. Free-form. */
  argHint?: string;
  /** `skill` | `project` | `plugin` | `builtin`. Advisory, for grouping. */
  source?: string;
  /** Absent means `prompt`. */
  kind?: CommandKind;
  /** For a control command, the config option it drives. */
  configId?: string;
  /**
   * Other names the agent answers to. `claude` gives `/usage` the aliases `cost`
   * and `stats`, and a menu that ignores them behaves unlike the terminal.
   */
  aliases?: string[];
}

export interface SessionSubagent {
  name: string;
  description?: string;
}

/** One choice within a `select` config option. */
export interface ConfigChoice {
  value: string;
  name?: string;
  description?: string;
}

/**
 * Something adjustable on a live session.
 *
 * The shape is ACP's `SessionConfigOption`. `category` is advisory grouping —
 * `model`, `effort`, `permission_mode`, or anything an agent invents — so the UI
 * groups by it without needing to know the vocabulary, and the next agent's knobs
 * render with no frontend change.
 */
export type ConfigOption =
  | {
      type: "select";
      id: string;
      name?: string;
      description?: string;
      category?: string;
      currentValue?: string | null;
      options: ConfigChoice[];
    }
  | {
      type: "boolean";
      id: string;
      name?: string;
      description?: string;
      category?: string;
      currentValue?: boolean;
    };

/** What a session's agent can do right now, read from its live link. */
export interface SessionCapabilities {
  commands: SessionCommand[];
  subagents: SessionSubagent[];
  /** Everything adjustable on this session, typed. */
  config: ConfigOption[];
  permissionMode?: string;
  contextWindow?: number;
}

/**
 * Whether a session has a live agent, and what it can do.
 *
 * `supports` is empty when detached, deliberately: capability is a property of a
 * live link, and reporting a remembered list would offer the user an interrupt
 * button for a session with no process to interrupt.
 */
export interface LinkState {
  attached: boolean;
  agent?: string;
  /** Closed vocabulary: `interrupt`, `steer`, `approval`, `subagent`, `slash`, … */
  supports: string[];
  capabilities?: SessionCapabilities;
  /**
   * Why there is no live link, when establishing one was tried and failed.
   *
   * Absent when a link is attached, and when nobody has asked for one yet. This
   * exists because an empty capability surface was ambiguous in a way that
   * mattered: an agent declaring no commands, an adapter that is not installed,
   * and a handshake still in flight all arrived as `{attached: false}`, so the UI
   * could only render nothing — which reads as a dead key rather than as a
   * missing install.
   */
  failure?: LinkFailure;
}

/** Why a link could not be established. */
export interface LinkFailure {
  /**
   * `no_adapter` — the session's agent was not discovered; a retry cannot fix it.
   * `handshake_failed` — the adapter would not come up; a retry may.
   *
   * Typed as an open union so an unknown code from a newer server still renders
   * through `message` instead of throwing away the whole reason.
   */
  code: "no_adapter" | "handshake_failed" | (string & {});
  message: string;
}

/** One option an agent offered on a request. */
export interface RequestOption {
  id: string;
  label: string;
  /** Whether choosing this is a lasting decision rather than a one-off. */
  persistent?: boolean;
  /**
   * What this option means, when the agent explained it.
   *
   * Present on a `choice` request, where the agent authored the options and its
   * own descriptions are most of what makes them readable — a picker listing
   * bare labels loses the reasoning the agent offered.
   */
  description?: string;
}

/**
 * A question an agent is blocked on.
 *
 * A resource rather than a stream message: the agent waits until someone
 * answers, so this survives a client disconnecting and is answerable from any
 * frontend — including one that connects later.
 */
export interface PendingRequest {
  id: string;
  sessionId: string;
  turnId: string;
  /** `approval` | `dialog` | `elicitation` | `choice`. */
  kind: string;
  tool?: string;
  toolUseId?: string;
  title: string;
  /** JSON: what the agent proposed. For an approval, the tool input. */
  detailJson: string;
  /** JSON array of `RequestOption`. */
  optionsJson: string;
  defaultOption?: string;
  /** `safe` | `destructive` | `network`. Advisory, for emphasis. */
  risk?: string;
  /**
   * Whether a lasting grant may be offered at all.
   *
   * `false` means the dialog must omit "always allow" entirely, not merely
   * de-emphasize it: the agent has said that granting always would write a rule
   * broader than this ask, so showing it turns a narrow yes into a wide one the
   * user never read. Absent means allowed.
   */
  allowPersistent?: boolean;
  /** When the agent stops waiting, if it will. Advisory. */
  expiresAtMs?: number;
  status: string;
  chosenOption?: string;
  createdAtMs: number;
  resolvedAtMs?: number;
}

/** Parse a request's proposed input. Never throws: a malformed payload renders
 * as no detail rather than breaking the dialog that has to display it. */
export function requestDetail(request: PendingRequest): Record<string, unknown> {
  try {
    const parsed = JSON.parse(request.detailJson);
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

/** Parse a request's offered options, falling back to a bare allow/deny.
 *
 * The fallback matters: a request whose options failed to parse would otherwise
 * render as a dialog with no buttons, leaving the agent blocked with no way for
 * the user to answer. Allow and deny are the two the backend validates against
 * whatever the agent actually offered, so a wrong guess is refused rather than
 * silently accepted.
 *
 * A persistent option is dropped when the request forbids one. That filtering lives
 * here rather than in the dialog so every consumer of this list is correct by
 * construction: the agent has said that granting always would write a permission
 * rule *broader than this ask*, and offering it anyway converts a narrow yes into a
 * wide one the user never read. Absent means allowed, which keeps a minimal adapter
 * working. */
export function requestOptions(request: PendingRequest): RequestOption[] {
  const offered = (() => {
    try {
      const parsed = JSON.parse(request.optionsJson);
      if (Array.isArray(parsed) && parsed.length > 0) return parsed as RequestOption[];
    } catch {
      // fall through
    }
    return [
      { id: "allow", label: "Allow" },
      { id: "deny", label: "Deny" },
    ] satisfies RequestOption[];
  })();

  if (request.allowPersistent === false) {
    const narrowed = offered.filter((option) => !option.persistent);
    // Filtering must never empty the dialog. If every option the agent offered was
    // persistent, a UI with no buttons leaves the agent blocked with no way for the
    // user to answer at all — strictly worse than the over-broad grant this filter
    // exists to avoid. Declining is always safe, and the backend validates the id
    // against what the agent offered, so a rejected deny fails loudly rather than
    // silently approving.
    return narrowed.length > 0 ? narrowed : [{ id: "deny", label: "Deny" }];
  }
  return offered;
}

export interface AgentCommand {
  description?: string;
  args?: Record<string, { type: string; required?: boolean; description?: string }>;
}

/**
 * Runner descriptors are passed through from `--describe` verbatim, so unlike
 * the database models these keep the runner protocol's snake_case spelling.
 */
export interface Agent {
  name: string;
  description?: string;
  session_key?: string;
  flags?: Record<string, AgentFlag>;
  commands?: Record<string, AgentCommand>;
}

/**
 * `GET /agents` and `POST /agents`.
 *
 * `broken` maps a runner directory name to why its `--describe` failed. Omitted
 * by the backend when empty, hence optional. It was previously absent from this
 * type, which is why a runner that failed to describe itself was invisible in the
 * settings panel — the one place a user looks to ask why their agent is missing.
 */
export interface AgentsResponse {
  agents: Agent[];
  broken?: Record<string, string>;
}

export interface CreateSessionRequest {
  agent: string;
  workdir: string;
  title?: string;
  origin?: SessionOrigin;
  /** Flag values (model, effort) applied to every turn in this session. */
  flags?: Record<string, string>;
  /** TraceDB session id to adopt when importing a native session. */
  traceSessionId?: string;
  /**
   * Whether this session gets Sylph's identity: `sylph` appends `SOUL.md`,
   * `preserve` runs the agent as it comes. Omitted means `sylph`, which is the
   * backend's default.
   *
   * Only meaningful at creation. Identity describes what the stored transcript
   * was produced by, so `PATCH /sessions` refuses it — see `settings.injectSoul`,
   * which is the preference this is derived from.
   */
  identityMode?: IdentityMode;
}

/** Identity policy for a session. Mirrors the backend's closed enum. */
export type IdentityMode = "sylph" | "preserve";

/**
 * `GET /soul`.
 *
 * `available: false` means no readable `SOUL.md` was resolved, so every session
 * runs on the agent's own identity whatever the client asks for. That is an
 * ordinary install state, not an error — which is why it arrives as a 200 with a
 * flag rather than a 404, and why `error` is a separate field: a file that exists
 * but cannot be read is a different problem with a different fix.
 */
export interface SoulResponse {
  available: boolean;
  path?: string;
  content?: string;
  bytes?: number;
  modifiedMs?: number;
  /** True when `content` holds only the first 128 KB. */
  truncated?: boolean;
  error?: string;
}

/** Attachment payload. `type` is the MIME type; `data` is base64, no data: prefix. */
export interface AttachmentPayload {
  name: string;
  type: string;
  data: string;
}

export interface CreateTurnRequest {
  text?: string;
  attachments?: AttachmentPayload[];
  idempotencyKey?: string;
  /** Named command from the agent's `--describe` output. */
  command?: string;
  args?: Record<string, string>;
}

/** Where in a session the query matched. */
export interface BestMatch {
  kind?: string;
  snippet?: string;
}

/**
 * One lineage-collapsed TraceDB search hit.
 *
 * `id` is the TraceDB session id (`claude:uuid`), not a Sylph Web session id.
 * `sessionId` is set only when this archived session has already been imported,
 * which is what makes a hit navigable instead of merely importable.
 */
export interface SearchResult {
  id: string;
  agent: string;
  title?: string;
  /** The opening user request: what makes a hit recognizable at a glance. */
  ask?: string;
  cwd?: string;
  outcome?: string;
  bestMatch?: BestMatch;
  /** How many events inside the session matched. */
  hits?: number;
  startedAtMs?: number;
  endedAtMs?: number;
  lineageRootId?: string;
  sessionId?: string;
}

export interface SearchResponse {
  results: SearchResult[];
  /**
   * False when the TraceDB archive could not be reached. The request still
   * succeeds, so the UI distinguishes "no matches" from "cannot search".
   */
  available: boolean;
  detail?: string;
}

/** Server-side filters for the session list. Omitted fields are no-ops. */
export interface SessionFilters {
  limit?: number;
  agent?: string;
  origin?: SessionOrigin;
  /** Case-insensitive substring match on the working directory. */
  workdir?: string;
  updatedAfterMs?: number;
  updatedBeforeMs?: number;
}

/** One working directory that groups sessions in the left sidebar. */
export interface Workspace {
  /** Absolute path. Display shortens it; every API call needs the real value. */
  path: string;
  name: string;
  sessionCount: number;
  runningCount: number;
  lastUsedMs: number;
  isQuickStart: boolean;
  /**
   * Archived sessions TraceDB holds here.
   *
   * Zero unless the request passed `includeArchived`, so zero means "not
   * counted" rather than "no history".
   */
  archivedCount: number;
}

export interface WorkspacesResponse {
  /**
   * Where Quick Start sessions run, resolved server-side.
   *
   * Server-side because the browser can neither expand `~` nor know what
   * `paths.quick_start_dir` was set to, so a client that derived it would be
   * wrong for anyone who configured one.
   */
  quickStartDir: string;
  dataDir: string;
  home: string;
  workspaces: Workspace[];
}

/** Which source supplied a resolved setting. */
export type ConfigSource = "env" | "file" | "default";

/** One resolved setting, as `GET /config` reports it. */
export interface ConfigEntry {
  /** Dotted key as it appears in `config.toml`, e.g. `paths.data_dir`. */
  key: string;
  value: string;
  source: ConfigSource;
  /** The environment variable that overrides it. */
  envVar: string;
  /**
   * Whether the path exists. Absent for settings that are not paths — which must
   * render differently from `false`, or a log level looks like a broken directory.
   */
  exists?: boolean;
}

/** One directory bundled-adapter resolution considered. */
export interface ConfigCandidate {
  path: string;
  exists: boolean;
  /** True for the one discovery actually reads. */
  chosen: boolean;
}

/**
 * What configuration resolved to, and who said so.
 *
 * Read-only by design: see the backend's `api::config` for why a loopback HTTP
 * surface must not be able to rewrite `browse_roots` or `agents_dir`. The panel
 * shows these values and the path of the file to edit.
 */
export interface ConfigReport {
  /** The one root everything else defaults under. */
  home: string;
  configPath: string;
  /** False before a first run has seeded it, or after a user deletes it. */
  configPresent: boolean;
  entries: ConfigEntry[];
  bundledAgentCandidates: ConfigCandidate[];
}

/** Which viewer a file wants. Absent means "read it as text". */
export type PreviewKind = "grid" | "blocks" | "html" | "pdf" | "image";

/** One entry in a session's file tree. */
export interface FileEntry {
  /** Path relative to the session workdir. The root itself is `""`. */
  path: string;
  name: string;
  isDir: boolean;
  size?: number;
  modifiedAtMs: number;
  isSymlink?: boolean;
  /**
   * Viewer family, classified server-side from the extension.
   *
   * Present so opening a file costs one request: the client knows which of
   * `content`, `document`, or `raw` to call before asking. Advisory — the
   * document endpoint validates the bytes and a mislabelled file falls back to
   * `content`.
   */
  previewKind?: PreviewKind;
}

export interface FileTreeResponse {
  /** Absolute path of the listed directory. */
  root: string;
  path: string;
  entries: FileEntry[];
  /** True when the directory held more entries than the server will list. */
  truncated: boolean;
}

/**
 * A previewable file. `encoding` decides the viewer, and is derived from the
 * bytes server-side rather than from the extension, so a `.log` holding binary
 * output is reported as binary rather than rendered as mojibake.
 */
export interface FileContentResponse {
  path: string;
  absolutePath: string;
  size: number;
  encoding: "text" | "image" | "binary";
  mimeType: string;
  /** Text, or base64 for images. Absent for binaries and oversized images. */
  content?: string;
  truncated: boolean;
  modifiedAtMs: number;
}

export interface ImportSessionRequest {
  /** TraceDB session id, formatted `<agent>:<native-id>`. */
  traceSessionId: string;
  title?: string;
}

/** What kind of value a spreadsheet cell holds. Drives column alignment. */
export type CellType = "text" | "number" | "bool" | "date" | "formula" | "error";

/**
 * One non-empty cell.
 *
 * `v` is rendered server-side, because that is where the number format lives: a
 * date arrives as an ISO string rather than the Excel serial the file stores.
 * `n` carries the raw number so sorting need not re-parse a formatted string.
 */
export interface Cell {
  v: string;
  t: CellType;
  n?: number;
  /** The formula expression, when the cell is computed. `v` stays its result. */
  f?: string;
}

export interface SheetInfo {
  name: string;
  /** The sheet's own size, not the window served. */
  rows: number;
  cols: number;
  /** False when the total was counted rather than read from the file. */
  exact?: boolean;
}

/** A window of one sheet. `null` cells are empty, which is most of a sheet. */
export interface GridPayload {
  sheets: SheetInfo[];
  sheet: number;
  /** First row of the window, absolute within the sheet. */
  row: number;
  /** Column the used range starts at, non-zero when data does not begin at A. */
  column: number;
  rows: (Cell | null)[][];
  hasHeaderRow: boolean;
  truncated: boolean;
}

/** A run of text with inline formatting. */
export interface Span {
  text: string;
  bold?: boolean;
  italic?: boolean;
}

/**
 * One block of document content.
 *
 * A discriminated union over `type`, shared by Word and PowerPoint so a single
 * renderer serves both.
 */
export type DocumentBlock =
  | { type: "heading"; level: number; text: string }
  | { type: "paragraph"; spans: Span[] }
  | { type: "listItem"; level: number; ordered: boolean; text: string }
  | { type: "table"; rows: string[][] };

/** One slide. Word documents have no sections. */
export interface DocumentSection {
  title?: string;
  index: number;
  /** Speaker notes, from the slide's own notes part. */
  notes?: string;
  /** Index into `blocks` where this section's content begins. */
  firstBlock: number;
}

export interface BlocksPayload {
  sections: DocumentSection[];
  blocks: DocumentBlock[];
  truncated: boolean;
}

/**
 * A parsed document. `kind` discriminates which payload is present.
 *
 * Parsed server-side: the npm option for these formats is unmaintained with
 * unpatched CVEs on the parse path, and would not fit the bundle budget anyway.
 */
export interface FileDocumentResponse {
  path: string;
  absolutePath: string;
  size: number;
  modifiedAtMs: number;
  kind: "grid" | "blocks";
  format: string;
  /** Display name for the header, e.g. "Excel workbook". */
  formatLabel: string;
  grid?: GridPayload;
  blocks?: BlocksPayload;
}

/** Window parameters for a spreadsheet request. Omitted fields take defaults. */
export interface DocumentWindow {
  sheet?: number;
  row?: number;
  rows?: number;
}

/**
 * One archived session, as the sidebar and history browser list it.
 *
 * `id` is a TraceDB session id (`claude:uuid`), not a Sylph Web session id — and
 * for a subagent it carries a `/agent-...` suffix, so it must be
 * percent-encoded into a URL path.
 */
export interface ArchiveSession {
  id: string;
  agent: string;
  /** Title, else the opening typed message. Absent for ~2% of real sessions. */
  label?: string;
  cwd: string;
  model?: string;
  startedAtMs?: number;
  endedAtMs?: number;
  /** Events the archive holds: the only size signal available. */
  events: number;
  /** Subagent sessions folded into this row rather than listed separately. */
  subagentCount: number;
  /**
   * Origin's TraceDB id when this session is a fork of another.
   *
   * A fork replays its origin's whole prefix, so the two read as one conversation
   * and only one belongs in a list. The server drops the loser within a single
   * response, but a pair can straddle two pages, so a client accumulating pages
   * must suppress a row whose `forkOf` names a session it has already shown.
   */
  forkOf?: string;
  /** Live session id when already imported, so the UI opens it instead. */
  sessionId?: string;
}

export interface ArchiveListResponse {
  sessions: ArchiveSession[];
  /** Cursor to continue from; absent on the last page. */
  nextCursor?: string;
  /**
   * False when the TraceDB archive could not be reached. The request still
   * succeeds, so a workspace with no archive reads as "live only" rather than
   * as an error.
   */
  available: boolean;
  detail?: string;
}

export interface BrowseRoot {
  /** Display label: the directory leaf, or `Home` for the home directory. */
  name: string;
  /** Absolute path, handed straight back as a `workdir` or a listing request. */
  path: string;
}

export interface BrowseRootsResponse {
  roots: BrowseRoot[];
  /** Absolute home directory, so paths can be abbreviated as `~/...`. */
  home: string;
}

/** A subdirectory in the machine-level browser. Files are never listed. */
export interface BrowseEntry {
  path: string;
  name: string;
  modifiedAtMs: number;
  isSymlink?: boolean;
}

export interface BrowseResponse {
  path: string;
  /**
   * Absent at a configured root.
   *
   * The server withholds it rather than the client hiding it, so a "go up"
   * affordance never offers a path the next request would refuse.
   */
  parent?: string;
  entries: BrowseEntry[];
  truncated: boolean;
}

export interface UploadFileRequest {
  /** Destination directory relative to the workdir. Empty means the root. */
  path?: string;
  name: string;
  /** Base64, no `data:` prefix — the same encoding attachments use. */
  data: string;
  /** Replace an existing file. Omitted means refuse instead. */
  overwrite?: boolean;
}

export interface UploadFileResponse {
  path: string;
  absolutePath: string;
  size: number;
}

export interface MkdirRequest {
  /** Parent directory relative to the workdir. Empty means the root. */
  path?: string;
  name: string;
}

export interface MkdirResponse {
  path: string;
  absolutePath: string;
}

/** A read-only archived transcript, normalized into live timeline events. */
export interface ArchiveTranscript {
  id: string;
  agent: string;
  label?: string;
  /** Absent for 19% of the archive, which is why a session may be unresumable. */
  cwd?: string;
  startedAtMs?: number;
  events: StreamEvent[];
  /** True when `limit` cut the stream short. */
  truncated: boolean;
  sessionId?: string;
}

/**
 * An API failure carrying the backend's stable error code.
 *
 * The backend answers errors as `{error: {code, message}}` with UPPER_SNAKE
 * codes; keeping `code` lets callers branch on machine-readable causes (for
 * example `SESSION_ALREADY_RUNNING`) instead of matching on prose.
 */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

/**
 * Parse a session's runner flags from the JSON column.
 *
 * The column is written by the backend and already validated, but parsed here
 * rather than duplicated across every row because JSON parsing can technically
 * throw and the client does not control what the editor-accessible database
 * stores it. Parsing it here means the composer reads `flags.model` without every
 * consumer repeating a `JSON.parse` that can throw on a hand-edited row.
 */
export function sessionFlags(session: Session): Record<string, string> {
  try {
    const parsed = JSON.parse(session.flagsJson || "{}");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    // Values are coerced rather than trusted: the column is free-form JSON, and a
    // non-string would otherwise reach a `<select value>` as `[object Object]`.
    return Object.fromEntries(
      Object.entries(parsed)
        .filter(([, value]) => typeof value === "string")
        .map(([key, value]) => [key, value as string]),
    );
  } catch {
    return {};
  }
}

async function fetchAPI<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`${API_BASE}${path}`, {
      ...init,
      credentials: "include",
      headers: {
        "Content-Type": "application/json",
        ...init?.headers,
      },
    });
  } catch {
    // fetch only rejects on transport failure, which locally means the backend
    // is not listening. Say so, instead of surfacing "Failed to fetch".
    throw new ApiError(
      0,
      "backend_unreachable",
      "Cannot reach the Sylph Web backend on port 4317. Start it with `cd backend && cargo run`.",
    );
  }

  if (!response.ok) {
    const body = await response.json().catch(() => null);
    const error = body?.error ?? {};
    // A missing envelope is itself diagnostic, so it gets its own message rather
    // than falling through to `statusText`. That fallback is what turned an
    // unreachable endpoint into the word "Not Found" rendered inside a file
    // tree: the pane said the *directory* was missing when the truth was that
    // the running backend had no files endpoint at all. Anything answering
    // `/api/v1/*` without the envelope is not a current Sylph backend — it is a
    // stale build, or a proxy answering in its place.
    if (!body?.error) {
      throw new ApiError(
        response.status,
        `http_${response.status}`,
        describeEnvelopeless(response.status, path),
      );
    }
    throw new ApiError(
      response.status,
      error.code ?? `http_${response.status}`,
      error.message ?? `Request failed (${response.status})`,
    );
  }

  if (response.status === 204) {
    return undefined as T;
  }

  return response.json() as Promise<T>;
}

/**
 * Explain a response that carried no error envelope.
 *
 * The backend documents every `/api/v1/*` failure as `{error: {code, message}}`,
 * including router-level ones, so a bodiless failure means the request never
 * reached a current backend. Naming that is the whole point: the alternative is
 * `statusText`, and "Not Found" alone sends the reader looking for a missing
 * file instead of a stale process.
 */
function describeEnvelopeless(status: number, path: string): string {
  if (status === 404) {
    return `The backend has no ${path} endpoint. It is probably an older build than this UI — restart it with \`cd backend && cargo run\`.`;
  }
  if (status === 502 || status === 503 || status === 504) {
    return "The backend did not answer. Check that it is running on the port the frontend proxies to.";
  }
  return `Request to ${path} failed (${status}) with no error details.`;
}

/**
 * Build a query string from filter values, dropping empty ones.
 *
 * Filters are applied server-side because the listing is `LIMIT`-capped:
 * filtering an already-truncated page would silently miss older matches.
 */
function queryString(params: Record<string, string | number | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === "") continue;
    search.set(key, String(value));
  }
  const encoded = search.toString();
  return encoded ? `?${encoded}` : "";
}

export const api = {
  sessions: {
    list: (filters: SessionFilters = {}) =>
      fetchAPI<{ sessions: Session[] }>(`/sessions${queryString({ ...filters })}`),
    get: (id: string) => fetchAPI<{ session: Session }>(`/sessions/${id}`),
    create: (req: CreateSessionRequest) =>
      fetchAPI<{ session: Session }>("/sessions", {
        method: "POST",
        body: JSON.stringify(req),
      }),
    /** Full durable history: every turn in the session with its events. */
    history: (id: string) =>
      fetchAPI<{ turns: TurnWithEvents[] }>(`/sessions/${id}/turns`),
    /**
     * Adopt an archived native session.
     *
     * Agent and working directory are derived from the archive by the backend,
     * so they are deliberately not accepted here.
     */
    import: (req: ImportSessionRequest) =>
      fetchAPI<{ session: Session }>("/sessions/import", {
        method: "POST",
        body: JSON.stringify(req),
      }),
    /** Change runner flags or the title of an existing session. */
    update: (id: string, req: { flags?: Record<string, string>; title?: string }) =>
      fetchAPI<{ session: Session }>(`/sessions/${id}`, {
        method: "PATCH",
        body: JSON.stringify(req),
      }),
    deleteEmpty: (id: string) =>
      fetchAPI<void>(`/sessions/${id}`, { method: "DELETE" }),
    /**
     * What this session's agent can do, read from a live handshake.
     *
     * Establishes a link if there is none, because the answer is only knowable
     * from a running agent — a project's command list depends on files the CLI
     * reads at startup. That makes this call slow the first time (seconds, while
     * the agent loads the project) and instant afterwards.
     */
    capabilities: (id: string) =>
      fetchAPI<LinkState>(`/sessions/${id}/capabilities`),
    /** Start the agent eagerly, before the user finishes typing. */
    attach: (id: string) =>
      fetchAPI<LinkState>(`/sessions/${id}/link`, { method: "POST" }),
    /** Stop the agent without ending the session. */
    detach: (id: string) =>
      fetchAPI<LinkState>(`/sessions/${id}/link`, { method: "DELETE" }),
    /** What the agent is currently blocked on. */
    requests: (id: string) =>
      fetchAPI<PendingRequest[]>(`/sessions/${id}/requests`),
    /**
     * End the current turn but keep the agent loaded.
     *
     * Distinct from `turns.stop`, which ends the agent too. These are different
     * intentions — "that is not what I meant" against "stop everything" — and the
     * UI should offer whichever the agent declares it supports.
     */
    interrupt: (id: string) =>
      fetchAPI<{ interrupted: boolean }>(`/sessions/${id}/interrupt`, {
        method: "POST",
      }),
    /**
     * Add text to a turn already in flight.
     *
     * `turnId` is a precondition, not a routing hint: a steer that races a turn
     * boundary would otherwise be delivered into the *next* turn, succeeding
     * silently with the text in the wrong conversation. A mismatch is a 409.
     */
    steer: (id: string, text: string, turnId?: string) =>
      fetchAPI<{ steered: boolean }>(`/sessions/${id}/steer`, {
        method: "POST",
        body: JSON.stringify({ text, turnId }),
      }),
    /**
     * Set one typed config option on a live session.
     *
     * Answers with the refreshed surface rather than an acknowledgement, because
     * setting one option can change another's choices — effort levels depend on
     * the selected model for both installed agents. So the response is applied
     * rather than an optimistic local value.
     */
    setConfig: (id: string, configId: string, value: string | boolean) =>
      fetchAPI<LinkState>(`/sessions/${id}/config`, {
        method: "POST",
        body: JSON.stringify({ configId, value }),
      }),
  },
  requests: {
    /**
     * Answer a request an agent raised.
     *
     * `input` carries an edited version of what the agent proposed, which is how
     * "allow, but change the command" works. Omitting it means "as proposed".
     *
     * Fails with `REQUEST_ALREADY_RESOLVED` when someone else answered first, and
     * that is worth surfacing rather than swallowing: it means the dialog on
     * screen was stale.
     */
    resolve: (requestId: string, option: string, input?: unknown) =>
      fetchAPI<PendingRequest>(`/requests/${requestId}/resolve`, {
        method: "POST",
        body: JSON.stringify({ option, input }),
      }),
  },
  files: {
    tree: (sessionId: string, path = "") =>
      fetchAPI<FileTreeResponse>(`/sessions/${sessionId}/files${queryString({ path })}`),
    content: (sessionId: string, path: string) =>
      fetchAPI<FileContentResponse>(`/sessions/${sessionId}/files/content${queryString({ path })}`),
    /**
     * Upload one file into the session's working directory.
     *
     * Confined server-side to the workdir subtree — the same range the agent
     * already writes to — and refuses to overwrite unless asked.
     */
    upload: (sessionId: string, req: UploadFileRequest) =>
      fetchAPI<UploadFileResponse>(`/sessions/${sessionId}/files`, {
        method: "POST",
        body: JSON.stringify(req),
      }),
    /** A spreadsheet, CSV, Word document, or deck as structure. */
    document: (sessionId: string, path: string, window: DocumentWindow = {}) =>
      fetchAPI<FileDocumentResponse>(
        `/sessions/${sessionId}/files/document${queryString({
          path,
          sheet: window.sheet,
          row: window.row,
          rows: window.rows,
        })}`,
      ),
    /**
     * URL for the file's bytes, for content the browser loads itself.
     *
     * A URL builder rather than a fetch: an `<img>`, `<iframe>`, or `<object>`
     * requests this itself, which is what buys progressive decode and HTTP
     * caching. Same-origin through the Next rewrite, so the `SameSite=Strict`
     * session cookie is sent.
     */
    rawUrl: (sessionId: string, path: string, options: { download?: boolean } = {}) =>
      `${API_BASE}/sessions/${sessionId}/files/raw${queryString({
        path,
        download: options.download ? "true" : undefined,
      })}`,
    /** Create one directory. Reports a collision rather than succeeding quietly. */
    mkdir: (sessionId: string, req: MkdirRequest) =>
      fetchAPI<MkdirResponse>(`/sessions/${sessionId}/files/directory`, {
        method: "POST",
        body: JSON.stringify(req),
      }),
    /**
     * Delete one file or directory. Irreversible: there is no trash to restore
     * from, which is why every caller confirms first.
     *
     * `recursive` is refused by default for a non-empty directory, so deleting a
     * subtree is a second, explicit answer rather than a silent consequence.
     */
    remove: (sessionId: string, path: string, options: { recursive?: boolean } = {}) =>
      fetchAPI<void>(
        `/sessions/${sessionId}/files${queryString({
          path,
          recursive: options.recursive ? "true" : undefined,
        })}`,
        { method: "DELETE" },
      ),
  },
  /**
   * Machine-level directory browsing, for choosing a workspace to bind.
   *
   * Separate from `files` because the root differs: `files` is rooted at one
   * session's workdir, this at the backend's configured browse roots. There is
   * deliberately no content call here — reading bytes stays session-scoped.
   */
  fs: {
    roots: () => fetchAPI<BrowseRootsResponse>("/fs/roots"),
    tree: (path = "") => fetchAPI<BrowseResponse>(`/fs/tree${queryString({ path })}`),
  },
  /**
   * The identity text Sylph appends to an agent's prompt.
   *
   * Read-only by design: editing `SOUL.md` is a filesystem act, and a loopback
   * HTTP surface that could rewrite a prompt preamble would be a much larger
   * security question than one that displays it.
   */
  soul: {
    get: () => fetchAPI<SoulResponse>("/soul"),
  },
  /**
   * What configuration resolved to, and which source supplied each value.
   *
   * Read-only, like `soul` above and for the same class of reason: `browseRoots`
   * bounds what any client of this backend may enumerate, and `agentsDir` names
   * directories the server executes programs from.
   */
  config: {
    get: () => fetchAPI<ConfigReport>("/config"),
  },
  workspaces: {
    /**
     * `includeArchived` also reports directories that exist only in the TraceDB
     * archive. Off by default because the sidebar polls this; surfaces where the
     * user *picks* a directory pass it, since a directory they want to work in
     * has no live session yet.
     */
    list: (options: { includeArchived?: boolean } = {}) =>
      fetchAPI<WorkspacesResponse>(
        `/workspaces${queryString({
          includeArchived: options.includeArchived ? "true" : undefined,
        })}`,
      ),
  },
  archive: {
    /**
     * Archived sessions that ran in one directory, newest first.
     *
     * This is what makes a workspace's past visible: `search` only answers a
     * question the user already knows how to ask.
     */
    list: (params: { workdir: string; limit?: number; cursor?: string }) =>
      fetchAPI<ArchiveListResponse>(`/archive/sessions${queryString({ ...params })}`),
    /**
     * One archived transcript, read-only.
     *
     * The id is encoded whole rather than interpolated: a subagent id contains a
     * `/`, which would otherwise be read as a path separator.
     */
    get: (traceId: string, limit?: number) =>
      fetchAPI<ArchiveTranscript>(
        `/archive/sessions/${encodeURIComponent(traceId)}${queryString({ limit })}`,
      ),
  },
  turns: {
    create: (sessionId: string, req: CreateTurnRequest) =>
      fetchAPI<{ turn: Turn }>(`/sessions/${sessionId}/turns`, {
        method: "POST",
        body: JSON.stringify(req),
      }),
    get: (turnId: string) => fetchAPI<{ turn: Turn }>(`/turns/${turnId}`),
    stop: (turnId: string) =>
      fetchAPI<{ turn: Turn }>(`/turns/${turnId}/stop`, { method: "POST" }),
  },
  agents: {
    list: () => fetchAPI<AgentsResponse>("/agents"),
    /**
     * Re-run `--describe` against every runner script.
     *
     * Worth being precise about what this does and does not do: it re-executes
     * the runner scripts, so a newly installed or newly fixed runner appears. It
     * does not re-read the agent's own configuration — a runner that prints its
     * model list as a literal in its `--describe` output keeps reporting the same
     * models until that script changes, however many providers were added to the
     * agent behind it. The settings panel says so, because otherwise "refreshed
     * and nothing changed" reads as a broken button.
     */
    refresh: () => fetchAPI<AgentsResponse>("/agents", { method: "POST" }),
  },
  search: (query: string, limit?: number) =>
    fetchAPI<SearchResponse>(`/search${queryString({ q: query, limit })}`),
  health: () =>
    fetchAPI<{
      ok: boolean;
      apiVersion: string;
      schemaVersion: number;
      /** Availability only: `/health` is unauthenticated, so it reports no counts. */
      tracedb: { available: boolean };
    }>("/health"),
};
