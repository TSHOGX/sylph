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
  workdir: string;
  /** Nullable: the backend stores title as an optional column. */
  title: string | null;
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
}

export interface WorkspacesResponse {
  /** Absolute `~/.agents`, resolved server-side: the browser cannot expand `~`. */
  quickStartDir: string;
  dataDir: string;
  home: string;
  workspaces: Workspace[];
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
  },
  workspaces: {
    list: () => fetchAPI<WorkspacesResponse>("/workspaces"),
  },
  files: {
    /** One directory level. Lazy expansion is what keeps a large repo usable. */
    tree: (sessionId: string, path = "") =>
      fetchAPI<FileTreeResponse>(
        `/sessions/${sessionId}/files${queryString({ path })}`,
      ),
    content: (sessionId: string, path: string) =>
      fetchAPI<FileContentResponse>(
        `/sessions/${sessionId}/files/content${queryString({ path })}`,
      ),
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
    rawUrl: (sessionId: string, path: string) =>
      `${API_BASE}/sessions/${sessionId}/files/raw${queryString({ path })}`,
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
