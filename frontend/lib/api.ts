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
  CreateSessionRequest, CreateTurnRequest, ControlRequest, DocumentWindow, EnumerateFilters,
  EnumerateResponse, EnvironmentsResponse, FileContentResponse, FileDocumentResponse,
  FileTreeResponse, LinkState, MkdirRequest, MkdirResponse, PendingRequest, SearchResponse,
  PairingGrant,
  PairingOffer,
  RereadResponse,
  Session,
  SessionEdge,
  SessionFilters,
  SoulResponse,
  Turn,
  HistoryPage,
  UploadFileRequest,
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

export async function fetchAPI<T>(path: string, init?: RequestInit): Promise<T> {
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
  } catch (error) {
    if (init?.signal?.aborted) throw error;
    // A transport failure identifies the broken request, not which machine or
    // process is down. Aborted queries have already retained their own error.
    throw new ApiError(
      0,
      "backend_unreachable",
      "Could not reach the Sylph API. Check the connection and try again.",
    );
  }

  if (!response.ok) {
    const body = await response.json().catch(() => null);
    const error = body?.error ?? {};
    // A missing envelope is itself diagnostic, so it gets its own message rather
    // than falling through to `statusText`. That fallback is what turned an
    // unreachable endpoint into the word "Not Found" rendered inside a file
    // tree: the pane said the *directory* was missing when the truth was that
    // the running backend had no files endpoint at all. A proxy timeout can also
    // omit this envelope while the backend is still processing the request.
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
 * including router-level ones. A proxy may replace that response or time out
 * before it arrives, so a bodiless failure must not imply a missing directory
 * or a crashed backend.
 */
function describeEnvelopeless(status: number, path: string): string {
  if (status === 404) {
    return `The backend has no ${path} endpoint. It is probably an older build than this UI — restart it with \`cd crates/server && cargo run\`.`;
  }
  if (status === 502 || status === 503 || status === 504) {
    return "The backend or its proxy did not answer in time. The request may still be running; check its status before retrying.";
  }
  if (status === 500) {
    return `The backend or its proxy could not complete ${path} and returned no details. Check the frontend and backend logs for a timeout or failure.`;
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

/**
 * Address a workspace file for either half of the session lifecycle.
 *
 * A draft handle is not an encoded session identity, so it must travel through the
 * draft file routes while the composer is still empty. Keeping this distinction in
 * the API client means panes, viewers, and upload operations do not each grow their
 * own `draft-` branch (and none of them can accidentally send the handle to a durable
 * endpoint again).
 */
function filesPath(id: string, suffix = ""): string {
  const root = id.startsWith("draft-") || id.startsWith("remote-draft-")
    ? `/sessions/draft/${segment(id)}/files`
    : `/sessions/${segment(id)}/files`;
  return `${root}${suffix}`;
}

export const api = {
  auth: {
    register: (name: string) =>
      fetchAPI<{ id: string; name: string; isAdmin: boolean; token: string }>("/auth/register", {
        method: "POST",
        body: JSON.stringify({ name }),
      }),
    login: (token: string) =>
      fetchAPI<{ id: string }>("/auth/login", {
        method: "POST",
        body: JSON.stringify({ token }),
      }),
    logout: () => fetchAPI<void>("/auth/logout", { method: "POST" }),
  },
  sessions: {
    list: (filters: SessionFilters = {}) =>
      fetchAPI<{ sessions: Session[] }>(`/sessions${queryString({ ...filters })}`),
    get: (id: string, signal?: AbortSignal) => fetchAPI<{ session: Session }>(`/sessions/${segment(id)}`, { signal }),
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
      fetchAPI<{
        draftId: string;
        environment: string;
        agent: string;
        workdir: string;
        title: string | null;
        injectIdentity: boolean;
      }>("/sessions", {
        method: "POST",
        body: JSON.stringify(req),
      }),
    preparedCapabilities: (draftId: string) =>
      fetchAPI<LinkState>(`/sessions/draft/${segment(draftId)}/capabilities`),
    preparedGet: (draftId: string) =>
      fetchAPI<{
        draftId: string;
        environment: string;
        agent: string;
        workdir: string;
        title: string | null;
        injectIdentity: boolean;
      }>(`/sessions/draft/${segment(draftId)}`),
    preparedConfig: (draftId: string, id: string, value: unknown) =>
      fetchAPI<LinkState>(`/sessions/draft/${segment(draftId)}/config`, {
        method: "POST",
        body: JSON.stringify({ id, value }),
      }),
    preparedTurn: (draftId: string, req: CreateTurnRequest) =>
      fetchAPI<{ session: Session }>(`/sessions/draft/${segment(draftId)}/turns`, {
        method: "POST",
        body: JSON.stringify(req),
      }),
    discardPrepared: (draftId: string) =>
      fetchAPI<void>(`/sessions/draft/${segment(draftId)}`, { method: "DELETE" }),
    history: (id: string, before?: string, signal?: AbortSignal) => {
      const query = new URLSearchParams({ limit: "100" });
      if (before) query.set("before", before);
      return fetchAPI<HistoryPage>(`/sessions/${segment(id)}/turns?${query}`, { signal });
    },
    eventBody: (id: string, seq: number) =>
      fetchAPI<{ text: string; data?: unknown }>(`/sessions/${segment(id)}/bodies/${seq}`),
    edges: (id: string) =>
      fetchAPI<{ edges: SessionEdge[] }>(`/sessions/${segment(id)}/edges`),
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
    capabilities: (id: string, signal?: AbortSignal) =>
      fetchAPI<LinkState>(`/sessions/${segment(id)}/capabilities`, { signal }),
    /** Start the agent eagerly, before the user finishes typing. */
    attach: (id: string) =>
      fetchAPI<LinkState>(`/sessions/${segment(id)}/link`, { method: "POST" }),
    /** Stop the agent without ending the session. */
    detach: (id: string) =>
      fetchAPI<LinkState>(`/sessions/${segment(id)}/link`, { method: "DELETE" }),
    /**
     * Parse the vendor's native store again, ingesting whatever the journal lacks.
     *
     * One pass. It starts no follower and does not change the session's `fidelity`, so
     * it is safe to repeat — and it is the only way a session adopted from a terminal
     * gains a line the vendor appended afterwards. Invalidating a query cache cannot:
     * that refetches what Sylph already holds, which is the defect this exists for.
     */
    reread: (id: string) =>
      fetchAPI<RereadResponse>(`/sessions/${segment(id)}/reread`, {
        method: "POST",
      }),
    reproject: (id: string) =>
      fetchAPI<RereadResponse>(`/sessions/${segment(id)}/reproject`, {
        method: "POST",
      }),
    /** What the agent is currently blocked on. */
    requests: (id: string, signal?: AbortSignal) =>
      fetchAPI<PendingRequest[]>(`/sessions/${segment(id)}/requests`, { signal }),
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
    steer: (id: string, text: string, turnId: string, idempotencyKey: string) =>
      fetchAPI<ControlRequest>(`/sessions/${segment(id)}/steer`, {
        method: "POST",
        body: JSON.stringify({ text, turnId, idempotencyKey }),
      }),
    /** Submit to the harness's own queue without claiming turn-scoped delivery. */
    nativeQueue: (id: string, text: string, priority: boolean, idempotencyKey: string) =>
      fetchAPI<ControlRequest>(`/sessions/${segment(id)}/native-queue`, {
        method: "POST", body: JSON.stringify({ text, priority, idempotencyKey }),
      }),
    queue: (id: string, text: string, idempotencyKey: string) =>
      fetchAPI<ControlRequest>(`/sessions/${segment(id)}/queue`, {
        method: "POST",
        body: JSON.stringify({ text, idempotencyKey }),
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
  controls: {
    list: (sessionId: string) =>
      fetchAPI<{ requests: ControlRequest[] }>(`/sessions/${segment(sessionId)}/control-requests`),
    edit: (sessionId: string, requestId: string, text: string, revision: number) =>
      fetchAPI<{ request: ControlRequest }>(`/control-requests/${segment(requestId)}`, {
        method: "PATCH",
        body: JSON.stringify({ text, revision }),
      }),
    cancel: (sessionId: string, requestId: string, revision: number) =>
      fetchAPI<{ request: ControlRequest }>(`/control-requests/${segment(requestId)}`, {
        method: "DELETE",
        body: JSON.stringify({ revision }),
      }),
    steer: (sessionId: string, requestId: string, revision: number, turnId: string, idempotencyKey: string) =>
      fetchAPI<{ request: ControlRequest }>(`/control-requests/${segment(requestId)}/steer`, {
        method: "POST",
        body: JSON.stringify({ revision, turnId, idempotencyKey }),
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
    tree: (sessionId: string, path = "", signal?: AbortSignal) =>
      fetchAPI<FileTreeResponse>(filesPath(sessionId, queryString({ path })), { signal }),
    content: (sessionId: string, path: string) =>
      fetchAPI<FileContentResponse>(filesPath(sessionId, `/content${queryString({ path })}`)),
    /**
     * Upload one file into the session's working directory.
     *
     * Confined server-side to the workdir subtree — the same range the agent
     * already writes to — and refuses to overwrite unless asked.
     */
    upload: (sessionId: string, req: UploadFileRequest) =>
      fetchAPI<UploadFileResponse>(filesPath(sessionId), {
        method: "POST",
        body: JSON.stringify(req),
      }),
    /** A spreadsheet, CSV, Word document, or deck as structure. */
    document: (sessionId: string, path: string, window: DocumentWindow = {}) =>
      fetchAPI<FileDocumentResponse>(
        filesPath(sessionId, "/document" + queryString({
          path,
          sheet: window.sheet,
          row: window.row,
          rows: window.rows,
        })),
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
      `${API_BASE}${filesPath(sessionId, "/raw" + queryString({
        path,
        download: options.download ? "true" : undefined,
      }))}`,
    /** Create one directory. Reports a collision rather than succeeding quietly. */
    mkdir: (sessionId: string, req: MkdirRequest) =>
      fetchAPI<MkdirResponse>(filesPath(sessionId, "/directory"), {
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
        filesPath(
          sessionId,
          queryString({
            path,
            recursive: options.recursive ? "true" : undefined,
          }),
        ),
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
    roots: (environment?: string) => fetchAPI<BrowseRootsResponse>(`/fs/roots${queryString({ environment })}`),
    tree: (path = "", environment?: string) => fetchAPI<BrowseResponse>(`/fs/tree${queryString({ path, environment })}`),
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
    createPairing: () =>
      fetchAPI<PairingOffer>("/environments/pairings", { method: "POST" }),
    pair: (request: {
      code: string;
      name?: string;
      origin?: "development" | "self-installed" | "ssh-bootstrapped" | "sandbox-provisioned";
      agents?: unknown[];
    }) =>
      fetchAPI<PairingGrant>("/environments/pair", {
        method: "POST",
        body: JSON.stringify(request),
      }),
    unpair: (id: string) =>
      fetchAPI<void>(`/environments/${segment(id)}`, { method: "DELETE" }),
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
