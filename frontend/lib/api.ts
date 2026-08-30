/**
 * API client for the Sylph Web backend.
 *
 * All requests go through the Next.js rewrite (`/api/v1/*` -> backend on 4317). Auth is
 * cookie-based: `GET /api/v1/auth/token` sets `sylph_session`, which the browser then
 * sends on every subsequent request. No explicit Authorization header is needed.
 *
 * The wire types live in `./api-types` and are re-exported here, so `from "@/lib/api"`
 * reaches either half and a caller wanting both keeps one import. See that file for why
 * the two are separate.
 */

export * from "./api-types";
import type {
  AdoptSessionRequest, BrowseResponse, BrowseRootsResponse, ConfigReport,
  CreateSessionRequest, CreateTurnRequest, DocumentWindow, EnumerateFilters,
  EnumerateResponse, EnvironmentsResponse, FileContentResponse, FileDocumentResponse,
  FileTreeResponse, LinkState, MkdirRequest, MkdirResponse, PendingRequest, SearchResponse,
  Session, SessionFilters, SoulResponse, Turn, TurnWithEvents, UploadFileRequest,
  UploadFileResponse, WorkspacesResponse,
} from "./api-types";

const API_BASE = "/api/v1";

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
 * Parse a session's spawn-time flags from the JSON column.
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

/**
 * A session id as one URL path segment.
 *
 * **Required, not tidiness.** The id is the encoded triple and its native part is a
 * vendor's value: `claude` names a delegated session
 * `<parent>/agent-a<name>-<hash>`, so a raw interpolation turns one segment into
 * three and the request reaches a route that does not exist — or worse, a different
 * one. The `:` delimiter also has meaning in the first segment of a relative
 * reference. Encoding here rather than at each call site is what stops one of a dozen
 * interpolations being forgotten.
 *
 * `encodeURIComponent` and never `encodeURI`: the latter deliberately leaves `/` and
 * `:` alone, which are precisely the two characters this exists to escape.
 *
 * **Exported because the streaming paths do not go through this client.** `use-agent-session`
 * opens its event stream with `fetch` directly, so it interpolates a session id itself — and
 * it did so raw, which is exactly the defect this function's first paragraph describes:
 * the browser percent-encoded the `:` on the way out, and the backend's `SessionId::decode`
 * splits on a *literal* `:` and never decodes, so it refused an id it had itself minted.
 * Exported rather than copied, because two copies of one escaping rule is one that will
 * diverge.
 */
export function segment(id: string): string {
  return encodeURIComponent(id);
}

export const api = {
  sessions: {
    list: (filters: SessionFilters = {}) =>
      fetchAPI<{ sessions: Session[] }>(`/sessions${queryString({ ...filters })}`),
    get: (id: string) => fetchAPI<{ session: Session }>(`/sessions/${segment(id)}`),
    /**
     * Start an agent in a directory.
     *
     * **This starts a process; it does not reserve a row.** A session has no identity
     * until an agent has assigned one, so there is nothing to create in advance —
     * which is what deleted the empty-draft class of session, the `DELETE` that
     * cleaned them up, and the window in which a session existed with no native id
     * and therefore could not be resumed.
     */
    create: (req: CreateSessionRequest) =>
      fetchAPI<{ session: Session }>("/sessions", {
        method: "POST",
        body: JSON.stringify(req),
      }),
    /** Full durable history: every turn in the session with its events. */
    history: (id: string) =>
      fetchAPI<{ turns: TurnWithEvents[] }>(`/sessions/${segment(id)}/turns`),
    /**
     * Bring in a session an agent already has on some environment.
     *
     * Replaces `POST /sessions/import`, which took one archive id. The working
     * directory and title are derived from the vendor's own store by the backend, so
     * they are deliberately not accepted here.
     */
    adopt: (req: AdoptSessionRequest) =>
      fetchAPI<{ session: Session }>("/sessions/adopt", {
        method: "POST",
        body: JSON.stringify(req),
      }),
    /**
     * What native sessions an environment holds, without adopting any.
     *
     * This is what makes {@link adopt} offerable at all: without it a user would have to
     * know a vendor's session id. A row already adopted arrives **marked** rather than
     * omitted — see {@link BlockedReason}.
     *
     * Slow on a first call and fast thereafter, and the asymmetry is worth knowing before
     * building a surface on it. Measured on a real machine, a cold enumerate costs
     * hundreds of milliseconds to seconds per agent — then tens of milliseconds per page while the
     * server's snapshot lives. So a picker should render its loading state honestly rather
     * than assume a listing is instant.
     */
    enumerate: (filters: EnumerateFilters) =>
      fetchAPI<EnumerateResponse>(`/sessions/enumerate${queryString({ ...filters })}`),
    /** Change agent flags or the title of an existing session. */
    update: (id: string, req: { flags?: Record<string, string>; title?: string }) =>
      fetchAPI<{ session: Session }>(`/sessions/${segment(id)}`, {
        method: "PATCH",
        body: JSON.stringify(req),
      }),
    /**
     * What this session's agent can do, read from a live handshake.
     *
     * Establishes a link if there is none, because the answer is only knowable
     * from a running agent — a project's command list depends on files the CLI
     * reads at startup. That makes this call slow the first time (seconds, while
     * the agent loads the project) and instant afterwards.
     *
     * **A failure here is a 200 with a reason, not a 5xx**, so the session stays
     * readable when its agent is gone. `lib/use-agent-session.ts` owns what that
     * means for caching.
     */
    capabilities: (id: string) =>
      fetchAPI<LinkState>(`/sessions/${segment(id)}/capabilities`),
    /** Start the agent eagerly, before the user finishes typing. */
    attach: (id: string) =>
      fetchAPI<LinkState>(`/sessions/${segment(id)}/link`, { method: "POST" }),
    /** Stop the agent without ending the session. */
    detach: (id: string) =>
      fetchAPI<LinkState>(`/sessions/${segment(id)}/link`, { method: "DELETE" }),
    /** What the agent is currently blocked on. */
    requests: (id: string) =>
      fetchAPI<PendingRequest[]>(`/sessions/${segment(id)}/requests`),
    /**
     * End the current turn but keep the agent loaded.
     *
     * Distinct from `turns.stop`, which ends the agent too. These are different
     * intentions — "that is not what I meant" against "stop everything" — and the
     * UI should offer whichever the agent declares it supports.
     */
    interrupt: (id: string) =>
      fetchAPI<{ interrupted: boolean }>(`/sessions/${segment(id)}/interrupt`, {
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
      fetchAPI<{ steered: boolean }>(`/sessions/${segment(id)}/steer`, {
        method: "POST",
        body: JSON.stringify({ text, turnId }),
      }),
    /**
     * Add text to run **after** the turn in flight.
     *
     * No `turnId`, and its absence is the difference rather than an omission: a queued
     * follow-up is *for* after the turn, so a boundary crossing underneath it changes
     * nothing. A steer carries the precondition because it is a correction and text that
     * misses its turn lands in the wrong conversation.
     *
     * Which of the two a session offers is `agent.can("steer")` against
     * `agent.can("queue")`, and they are genuinely different per vendor: `claude` has only
     * queueing — its stdin is read once the turn ends — while `pi` and `codex` have both.
     */
    queue: (id: string, text: string) =>
      fetchAPI<{ queued: boolean }>(`/sessions/${segment(id)}/queue`, {
        method: "POST",
        body: JSON.stringify({ text }),
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
      fetchAPI<LinkState>(`/sessions/${segment(id)}/config`, {
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
      fetchAPI<FileTreeResponse>(`/sessions/${segment(sessionId)}/files${queryString({ path })}`),
    content: (sessionId: string, path: string) =>
      fetchAPI<FileContentResponse>(`/sessions/${segment(sessionId)}/files/content${queryString({ path })}`),
    /**
     * Upload one file into the session's working directory.
     *
     * Confined server-side to the workdir subtree — the same range the agent
     * already writes to — and refuses to overwrite unless asked.
     */
    upload: (sessionId: string, req: UploadFileRequest) =>
      fetchAPI<UploadFileResponse>(`/sessions/${segment(sessionId)}/files`, {
        method: "POST",
        body: JSON.stringify(req),
      }),
    /** A spreadsheet, CSV, Word document, or deck as structure. */
    document: (sessionId: string, path: string, window: DocumentWindow = {}) =>
      fetchAPI<FileDocumentResponse>(
        `/sessions/${segment(sessionId)}/files/document${queryString({
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
      `${API_BASE}/sessions/${segment(sessionId)}/files/raw${queryString({
        path,
        download: options.download ? "true" : undefined,
      })}`,
    /** Create one directory. Reports a collision rather than succeeding quietly. */
    mkdir: (sessionId: string, req: MkdirRequest) =>
      fetchAPI<MkdirResponse>(`/sessions/${segment(sessionId)}/files/directory`, {
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
        `/sessions/${segment(sessionId)}/files${queryString({
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
   * bounds where a client of this backend may cause arbitrary code to run.
   */
  config: {
    get: () => fetchAPI<ConfigReport>("/config"),
  },
  /**
   * Directories that group sessions, and where Quick Start runs.
   *
   * One query, because there is one store. This used to take `includeArchived` to
   * fold in directories that existed only in the separate archive, and there is no
   * longer a second place a directory can be recorded.
   */
  workspaces: {
    list: () => fetchAPI<WorkspacesResponse>("/workspaces"),
  },
  turns: {
    create: (sessionId: string, req: CreateTurnRequest) =>
      fetchAPI<{ turn: Turn }>(`/sessions/${segment(sessionId)}/turns`, {
        method: "POST",
        body: JSON.stringify(req),
      }),
    get: (turnId: string) => fetchAPI<{ turn: Turn }>(`/turns/${turnId}`),
    stop: (turnId: string) =>
      fetchAPI<{ turn: Turn }>(`/turns/${turnId}/stop`, { method: "POST" }),
  },
  /**
   * Every paired environment, whether it is connected, and what it can run.
   *
   * **This replaces `GET /agents`, and the replacement is not a rename.** There is
   * no global agent list any more: which agents exist is decided per machine by what
   * is on its `PATH`, so a merged list would be true of nowhere. Every consumer that
   * used to read one list now has to say *which environment* it is asking about.
   *
   * There is likewise no `POST /agents` refresh. That re-ran a discovery pass over
   * adapter directories; availability is now `PATH` plus a version, reported by the
   * envagent that holds it, so there is no pass on this side to re-run.
   */
  environments: {
    list: () => fetchAPI<EnvironmentsResponse>("/environments"),
  },
  /**
   * FTS5 over the server's own history.
   *
   * **Keep this off any polled path.** Measured against an equivalent store, every
   * other read is microseconds to sub-millisecond while this is orders of magnitude slower — so
   * gating it on a real query length is not politeness, it is the difference between
   * a search box and a load generator.
   */
  search: (query: string, limit?: number) =>
    fetchAPI<SearchResponse>(`/search${queryString({ q: query, limit })}`),
  health: () =>
    fetchAPI<{
      ok: boolean;
      apiVersion: string;
      schemaVersion: number;
      /**
       * Per environment, because "the agent will not start" has a different cause and a
       * different fix per machine — one global boolean cannot distinguish a missing CLI
       * from a sleeping laptop from an exhausted sandbox node.
       *
       * `/health` is unauthenticated, so it reports availability and never counts.
       */
      environments: {
        id: string;
        name: string;
        connected: boolean;
        disconnectedSinceMs?: number;
        agents: { name: string; version: string }[];
      }[];
    }>("/health"),
};
