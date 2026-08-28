/**
 * API client for the Sylph Web backend.
 *
 * All requests go through the Next.js rewrite (`/api/v1/*` -> backend on 4317).
 * Auth is cookie-based: `GET /api/v1/auth/token` sets `sylph_session`, which the
 * browser then sends on every subsequent request. No explicit Authorization
 * header is needed.
 *
 * The backend serializes every model with `#[serde(rename_all = "camelCase")]`,
 * so wire fields are `createdAtMs`, `injectIdentity`, `requestText` and so on.
 * These interfaces mirror the wire shape exactly; do not "tidy" them into
 * snake_case, because that silently yields `undefined` at runtime rather than a
 * type error.
 */

const API_BASE = "/api/v1";

export type SessionStatus = "idle" | "running";

/**
 * How much resolution the record of a session has.
 *
 * Mirrors `Fidelity` in `crates/protocol/src/identity.rs`, whose wire spelling is
 * `snake_case`. **A property of the record, not of who started the session**: a
 * session Sylph launched, read back from the vendor's native store after the
 * process died, is `messages` too. So this cannot be inferred from anything about a
 * session's origin, and there is no origin field to infer it from.
 *
 * **A renderer reads this before drawing a duration.** `stream` means every delta
 * and its arrival time was recorded; the other two mean the source recorded less,
 * and no care downstream recovers a timing that was never written — see
 * `lib/timeline-waterfall.ts`, which refuses to lay out an axis without it.
 */
export type Fidelity = "stream" | "blocks" | "messages";

export interface Session {
  /**
   * The encoded identity triple, `<environment>:<agent>:<nativeId>`.
   *
   * **Opaque by contract** (`docs/api-design.md` § A session id in a path is the
   * identity triple): echo it back, never parse it. The three parts are separate
   * fields below precisely so nothing has to — reading them out of the id is how an
   * encoding change breaks a client with no error at any layer. Note also that the
   * delimiter is `:` and a native id may itself contain one, so a client that split
   * this would be wrong even about the id it was handed.
   */
  id: string;
  /** Opaque environment id, minted at pairing. Never a hostname. */
  environment: string;
  /**
   * Display name for {@link environment}. Mutable and never a key: a rename must
   * not destroy identity (`docs/environments.md` § Pairing).
   */
  environmentName: string;
  agent: string;
  /** The vendor's own id for this session, unchanged. */
  nativeId: string;
  workdir: string;
  /** Nullable: the backend stores title as an optional column. */
  title: string | null;
  /**
   * Whether `SOUL.md` was appended when this session started.
   *
   * Settable at creation and refused by `PATCH`, because a transcript records what
   * produced it — rewriting the flag would rewrite what the stored history means.
   */
  injectIdentity: boolean;
  fidelity: Fidelity;
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
  /**
   * Who started this turn.
   *
   * `agent` means nobody did: the agent spoke after its previous turn had settled,
   * which is how backgrounded work reports finishing. Such a turn has an empty
   * `requestText` by construction, so this is the only thing that distinguishes it
   * from a user who sent nothing — and without the distinction it reads as a reply to
   * a message the user never wrote.
   *
   * Required: `backend/src/models/turn.rs:20` has this as a non-`Option` `String`
   * with no `skip_serializing_if`, so it is on every row the backend serializes.
   */
  origin: "user" | "agent";
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
  /**
   * When a consolidated block's *first* phase arrived. Absent on a live event.
   *
   * A live event is one arrival, so `createdAtMs` is the whole story. A consolidated
   * block is many arrivals folded into one row keyed by its close, which makes
   * `createdAtMs` its *end* — so this is the only field that says when it began.
   * Presence therefore also distinguishes settled history from a live stream.
   */
  startedAtMs?: number;
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
 * Why one facet of a link is unavailable.
 *
 * **Closed, mirroring `UnavailableReason` in `crates/protocol/src/link.rs` exactly**
 * (wire spelling `snake_case`). Leaving it open with `(string & {})` for "a newer
 * server" — which there is no such thing as here — costs the two sites that branch
 * on it their exhaustiveness, and both make a decision a wrong default gets wrong:
 * `components/composer.tsx` decides retryability and what to say, and
 * `lib/use-agent-session.ts` decides cache lifetime. A variant added to the protocol
 * should fail the type check rather than silently fall to whichever arm handles the
 * rest.
 *
 * Note what changed with the transport, because two of these names are new and one
 * old name is gone. There is no adapter to be absent — adapters are linked into the
 * envagent, and an agent's availability is whether its CLI is on that machine's
 * `PATH` — so `no_adapter` is now `no_agent`. And the two old codes both described
 * *our own spawn* failing, so neither could say "somebody else's process, running
 * fine, not ours to drive", which is the common case now.
 */
export type UnavailableReason =
  /** This agent's CLI is not on that machine's `PATH`. An install, not a retry. */
  | "no_agent"
  /**
   * No envagent is connected for this session's environment.
   *
   * **Observe still answers from the server's store**, which is why the two facets
   * are separate: history reads while nothing can be sent.
   */
  | "environment_unreachable"
  /**
   * The process is alive under someone else, and this vendor offers a second party
   * no control channel. A terminal-born `pi` session is the measured case.
   */
  | "not_ours_to_drive"
  /** The channel exists and would not come up. **The one reason a retry can fix.** */
  | "handshake_failed";

/**
 * Whether trying again could change the answer.
 *
 * Mirrors `UnavailableReason::is_retryable` in `crates/protocol/src/link.rs`, which
 * declares the rule so there is one answer rather than a condition buried inside a
 * query's `staleTime` where a second consumer cannot read it. No retry installs a
 * CLI, wakes a sleeping laptop, or teaches a vendor a new channel.
 *
 * Exhaustive by construction: the parameter is the closed union, so a variant added
 * to the protocol fails the type check here rather than falling through as
 * retryable.
 */
export function isRetryable(reason: UnavailableReason): boolean {
  switch (reason) {
    case "handshake_failed":
      return true;
    case "no_agent":
    case "environment_unreachable":
    case "not_ours_to_drive":
      return false;
  }
}

/**
 * One facet of a link: available, or unavailable with a reason.
 *
 * Not a bare boolean, and that is the original defect rather than a refinement: the
 * server logged the reason and returned `attached: false`, so a client could not
 * distinguish a failed handshake from an agent that simply declares no commands, and
 * the only honest thing it could render was nothing at all. That is how a stale
 * install presented as a composer ignoring the `/` key.
 *
 * `reason` is populated exactly when `available` is false. A client reads
 * `available` first and `reason` only on the false branch — the invariant is the
 * server's (`Facet`'s constructors in `crates/protocol/src/link.rs`), not this
 * type's, because this shape is what the wire carries.
 */
export interface Facet {
  available: boolean;
  reason?: UnavailableReason;
}

/**
 * A link's two facets.
 *
 * **One state, two facets** — not three link states (`docs/architecture.md` § One
 * link, two typed capability facets). The vendors do not sort into three buckets: a
 * terminal `pi` is observable and undrivable, `codex` is both, `claude` is
 * observable and semi-drivable behind an approval gate. A third state would have to
 * pick one and misreport the others.
 */
export interface LinkFacets {
  /** Can Sylph see what this session is doing. */
  observe: Facet;
  /** Can Sylph send this session a turn. */
  control: Facet;
}

/**
 * What Sylph can do with a session right now, and what it cannot.
 *
 * `supports` is empty when there is no live link, deliberately: capability is a
 * property of a link, and reporting a remembered list would offer an interrupt
 * button for a session with no process to interrupt.
 *
 * **An empty `capabilities` means "Sylph cannot reach it", never "there is nothing
 * there."** Those coincide for a session Sylph launched and are opposite for one
 * running in a terminal, where the process is alive and simply not ours to drive —
 * so a renderer must read {@link link} before concluding anything from the surface's
 * emptiness. `components/composer.tsx`'s `SlashStatus` is the surface that has to
 * get this right, because the two cases have different sentences and one of them has
 * a retry button.
 */
export interface LinkState {
  link: LinkFacets;
  agent?: string;
  /** Closed vocabulary: `interrupt`, `steer`, `approval`, `subagent`, `slash`, … */
  supports: string[];
  capabilities?: SessionCapabilities;
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
  /**
   * JSON: the input this request declared it wants collected, when it declares one.
   *
   * Absent is an approval, whose subject is one proposed tool input rendered from
   * `detailJson`. Parsed by `requestFormOf` in `lib/request-form.ts`; see
   * `docs/session-protocol.md` § Request forms for why it is declared rather than
   * inferred from `detailJson`'s field names.
   */
  formJson?: string;
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

/**
 * An agent CLI available on one environment.
 *
 * **There is no global agent list.** Which agents exist is decided per environment
 * by what is on that machine's `PATH` (`docs/api-design.md` § Agents and
 * environments), so a merged list would be true of nowhere — and a user picking an
 * environment with no `codex` on it should learn that before creating the session
 * rather than after. This type is therefore only ever reached through an
 * {@link Environment}.
 *
 * There is likewise no `broken` map: availability is `PATH`, not a discovery pass
 * that can half-succeed. "Why can I not use this agent" now has one answer with one
 * fix, which is naming the CLI that was not found.
 */
export interface Agent {
  name: string;
  version: string;
  /**
   * The spawn-time option schema: what can be chosen *before* a session exists.
   *
   * Anything adjustable on a live session is a config facet instead, reported by
   * `GET /sessions/{id}/capabilities` — which is also where a session's real slash
   * commands arrive, because those depend on files the CLI reads at startup.
   */
  flags?: Record<string, AgentFlag>;
  /** Closed vocabulary, drives the UI directly. Same as {@link LinkState.supports}. */
  supports: string[];
}

/**
 * How an envagent came to exist, which decides what AFK can promise.
 *
 * Closed for the same reason {@link UnavailableReason} is: it is protocol, and a
 * value added upstream should fail the type check rather than render as an unlabelled
 * environment.
 */
export type EnvironmentOrigin =
  /** A user installed the envagent and paired it. A laptop, a build box. */
  | "self-installed"
  /** An existing envagent copied the binary to a host it can reach over SSH. */
  | "ssh-bootstrapped"
  /** The server asked Khora for a sandbox whose image contains an envagent. */
  | "sandbox-provisioned";

/** One paired environment: whether it is connected, and what it can run. */
export interface Environment {
  /** Opaque id, minted at pairing. Never a hostname, never rendered as one. */
  id: string;
  /** Display name. Mutable, and never a key. */
  name: string;
  connected: boolean;
  origin: EnvironmentOrigin;
  /**
   * The environment relaying for this one, when it is not reachable directly.
   *
   * **Not decoration.** A non-null value means this environment is reachable only
   * while the named one relays for it, so **control dies when that machine leaves**
   * (`docs/environments.md` § SSH-bootstrapped). A UI rendering it identically to a
   * directly-connected environment would promise AFK continuity it cannot keep,
   * which is the whole reason the field is on the wire.
   */
  reachableVia: string | null;
  agents: Agent[];
}

export interface EnvironmentsResponse {
  environments: Environment[];
}

export interface CreateSessionRequest {
  /** Which machine to run on. Required: a session has no default environment. */
  environment: string;
  agent: string;
  workdir: string;
  title?: string;
  /** Flag values (model, effort) applied to every turn in this session. */
  flags?: Record<string, string>;
  /**
   * Whether `SOUL.md` is appended. Defaults to `true` server-side.
   *
   * Only meaningful at creation, and **refused by `PATCH`**: identity describes what
   * the stored transcript was produced by. See `settings.injectSoul`, the preference
   * this is derived from.
   */
  injectIdentity?: boolean;
}

/**
 * Bring in a session an agent already has, from that agent's own native store.
 *
 * Identity is the triple, so all three parts are named rather than one archive id
 * being handed over. Everything policy-relevant — the working directory, the title —
 * is derived from the store by the backend rather than accepted here.
 */
export interface AdoptSessionRequest {
  environment: string;
  agent: string;
  nativeId: string;
  /**
   * Follow the store as it grows, so a session running in a terminal right now
   * appears and keeps updating. `false` parses one pass of what is already recorded.
   */
  watch?: boolean;
  title?: string;
  /**
   * Defaults to **false** here, unlike on create: the transcript so far was produced
   * without Sylph's identity. Settable, because adopting a terminal session and
   * injecting from that point on is a coherent thing to want.
   */
  injectIdentity?: boolean;
}

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
 * One search hit: a session, and why it matched.
 *
 * **One store, so a hit is a session rather than a foreign row.** This used to be
 * archive-shaped — a TraceDB id, a `cwd`, and a `sessionId` set only when the
 * archived row had already been imported, which is what made a hit *importable*
 * rather than navigable. All of that is gone: the index is over the same rows the
 * timeline renders (`docs/database-schema.md` § Search lives beside `history.db`), so
 * every hit is openable and there is no second id space to translate through.
 *
 * The session is carried whole rather than flattened into a handful of fields, so a
 * result row renders with the same `sessionLabel`, fidelity marker, and environment
 * chip a listing row does — and gains whatever is added to {@link Session} without a
 * change here.
 */
export interface SearchResult {
  session: Session;
  bestMatch?: BestMatch;
  /** How many blocks inside the session matched. */
  hits?: number;
}

/**
 * `GET /search`.
 *
 * No `available` flag and no `detail`. Those existed because search ran against a
 * separate process that could be absent, so "no matches" and "cannot search" had to
 * be distinguishable; the store is owned in-process now and an unreachable index is
 * not a state this endpoint has. It never 503s and answers an empty array for a
 * query that finds nothing (`docs/api-design.md` § Search).
 */
export interface SearchResponse {
  results: SearchResult[];
}

/** Server-side filters for the session list. Omitted fields are no-ops. */
export interface SessionFilters {
  limit?: number;
  agent?: string;
  /** Opaque environment id. */
  environment?: string;
  /**
   * Working directory.
   *
   * **An exact match after normalizing trailing slashes, never a substring.** The
   * store this replaced compiled it to `LIKE '%' || ? || '%'`, so asking for `/w/app`
   * also returned `/w/app-old` and every subdirectory.
   */
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

/**
 * What configuration resolved to, and who said so.
 *
 * Read-only by design: see the backend's `api::config` for why a loopback HTTP surface
 * must not be able to rewrite `browse_roots`, which bounds where a spawned agent's
 * `current_dir` may be and therefore where arbitrary code may run. The panel shows
 * these values and the path of the file to edit.
 *
 * There is no `bundledAgentCandidates` any more, and no `tracedb` row: adapters are
 * linked into the envagent rather than discovered in directories, and a setting naming
 * an external archive binary is only meaningful while an external binary answers.
 */
export interface ConfigReport {
  /** The one root everything else defaults under. */
  home: string;
  configPath: string;
  /** False before a first run has seeded it, or after a user deletes it. */
  configPresent: boolean;
  entries: ConfigEntry[];
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
   * other read is microseconds to sub-millisecond while this is 174–260 ms — so
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
       * Per environment, because "the agent will not start" has a different cause
       * and a different fix per machine. This replaces a single `tracedb.available`
       * boolean, which could not distinguish a missing CLI from a sleeping laptop.
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
