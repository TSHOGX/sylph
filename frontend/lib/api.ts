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

/** A flag declared by a runner's `--describe` output. */
export interface AgentFlag {
  choices: string[];
  description?: string;
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

export interface CreateSessionRequest {
  agent: string;
  workdir: string;
  title?: string;
  origin?: SessionOrigin;
  /** Flag values (model, effort) applied to every turn in this session. */
  flags?: Record<string, string>;
  /** TraceDB session id to adopt when importing a native session. */
  traceSessionId?: string;
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
  /** Absolute `~/.agents`, resolved server-side: the browser cannot expand `~`. */
  quickStartDir: string;
  dataDir: string;
  home: string;
  workspaces: Workspace[];
}

/** One entry in a session's file tree. */
export interface FileEntry {
  /** Path relative to the session workdir. The root itself is `""`. */
  path: string;
  name: string;
  isDir: boolean;
  size?: number;
  modifiedAtMs: number;
  isSymlink?: boolean;
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
    list: () => fetchAPI<{ agents: Agent[] }>("/agents"),
    refresh: () => fetchAPI<{ agents: Agent[] }>("/agents", { method: "POST" }),
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
