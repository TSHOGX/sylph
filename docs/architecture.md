# Architecture

Sylph Web is a local-first native UI for coding-agent sessions. It orchestrates agent
runtimes through a unified interface while preserving their native identities and
lifecycle policies. Two adapters ship, `claude` and `pi`; archived history covers
`codex` as well, which opens read-only.

## System Overview

```
Browser (127.0.0.1:3000)
    │
    │ HTTP + SSE
    ▼
Next.js Frontend
    │ API proxy
    ▼
Rust Backend (127.0.0.1:4317)
    │
    ├─► journal.db (SQLite) ─────┐
    │   Unsettled turns          │
    ├─► history.db + blobs/      │
    │   The durable record       ├─► Query & Search
    ├─► archive.db ──────────────┘
    │   Agent-native history, ingested
    │
    └─► Adapters (child processes)
        claude, pi
```

**Components:**
- **Frontend** — Next.js 16 + React 19, handles UI and real-time updates
- **Backend** — Rust + Axum, REST API + SSE streaming, process supervision
- **Journal** — SQLite (journal.db), sessions and turns that have not settled
- **History** — SQLite (history.db) plus `blobs/`, the durable replayable record
- **Archive** — SQLite (archive.db), agent-native history parsed and ingested by the
  embedded TraceDB crate, searchable across agents
- **Adapters** — An `adapter.sh` per agent, discovered in two roots and spawned per
  session, speaking `sylph.agent.v3` on stdio

## Design Principles

1. **Local-first** — Data lives with the server that owns it, and no third-party service
   is in the path. "Local" is a statement about dependencies rather than about geography:
   a Sylph server deployed on a remote host is still local-first, because the authority is
   the server's own store and not somebody else's API.
2. **Origin as policy** — Session origin (`sylph` or `native`) determines identity and lifecycle
3. **The server owns the record** — An agent still writes its native format, and TraceDB
   still ingests it where it can be reached. But the server's normalized event stream is
   the authority, because the server is the one component present for all work regardless
   of where it runs — a sandbox's native store dies with the sandbox, and an archive
   derived only from native stores acquires a hole exactly there. See
   `docs/data-architecture.md`.
4. **Bounded journal, durable history** — Unsettled turns in journal.db, consolidated
   into history.db and pruned only when a downstream authority confirms the data
5. **Durable turn supervision** — Turn execution independent of HTTP request lifecycle

## Session Model

### Session Origins

| Origin | Created by | Lifecycle | SOUL.md injection |
|--------|-----------|-----------|-------------------|
| `sylph` | WebApp | 24h idle / 04:00 reset — **policy, not yet implemented** | Per `identityMode`, default yes |
| `native` | CLI/TUI import | No implicit reset | Never |

**Invariants:**
- Origin is immutable after creation
- Native sessions never receive SOUL.md or lifecycle resets
- Sylph sessions are *intended* to follow a managed lifecycle (24h idle, 04:00 cutoff)

> **The lifecycle half of this model is not enforced by the running system.**
> `identity_mode` is read and acted on (`links/mod.rs:444` resolves `SYLPH_SOUL` from
> it), so the native-session identity invariant holds and is tested. `lifecycle_mode` is
> written to the journal and **read by nothing**: `end_incarnation` has no callers, no
> timer or scheduler pass retires an incarnation, and neither the 24-hour idle reset nor
> the 04:00 cutoff fires. Read this section as the policy the data model was built for.
>
> Note what the exposure is and is not. A native session is not at risk — the reset that
> would violate its invariant happens to nobody. The consequence is the reverse: a Sylph
> session keeps one agent context indefinitely. Do not conflate this with the **link**
> idle timeout, which *is* implemented (30 minutes, `links/mod.rs:211`) and is a
> different thing: a link is a live adapter process, an incarnation is the agent's
> conversational context. `docs/roadmap.md` § Audit, 2026-08-26 carries the measurement
> and the open question about whether an automatic reset is even the right thing to
> build.

### Origin already names the class that matters, and the durable key includes environment

`origin` is the axis `docs/data-architecture.md` calls **Sylph-born** versus
**adopted**, so there is no third vocabulary: `sylph` means Sylph spawned the adapter
and holds its stdio, `native` means somebody else started the process and Sylph reads
it through the agent's own store. Naming it twice would be worse than naming it once —
what the document adds is the consequence, which is that the two classes answer
*every* later question differently: what can be observed, what can be driven, which
store is authoritative, and whether upload is automatic or explicit.

The durable key is `(environment, agent, native_id)`, with Sylph minting the UUID that
the API exposes on first sighting. The archive id cannot be the key, because a session
is observable before anything has ingested it. A content fingerprint cannot be either:
it is a fact about a file, and an agent is free to keep no per-session file at all. The key is sufficient only because **a session never moves
between environments** — that invariant is what removes every cross-environment
freshness comparison, and it is also why `cwd` is unambiguous despite two machines
plausibly both having `/home/user/app`.

### Identity is a second axis, not a synonym for origin

`identityMode` is `sylph` (inject `SOUL.md`) or `preserve` (run the agent on its own
identity). It is chosen when a session is created, defaults to `sylph`, and `PATCH`
refuses it — identity describes what a stored transcript was *produced by*, so
changing it on a session with history would rewrite what that history means.

**A Sylph session that declines injection is still a Sylph session.** Origin stays
`sylph`, and managed lifecycle still governs it in policy. Collapsing the two axes would
make "run this agent as itself" indistinguishable from an imported session that must
never be reset or compacted — the invariant above. So:

| | `identityMode: sylph` | `identityMode: preserve` |
| --- | --- | --- |
| `origin: sylph` | The default. Injects; resets when reset is built. | Agent's own identity, same lifecycle class. |
| `origin: native` | Never produced. | Every imported session. |

The two axes remain worth separating even while one is unenforced — arguably
*especially* then. If they were one field, building the reset later would mean deriving
"may I reset this" from a value that also decides injection, and the first session to
decline injection would silently become unresettable.

An import sets both `identityMode` and `lifecycleMode` to `preserve`, derived from
the archive rather than the request. `GET /api/v1/soul` serves the text this
governs, so the toggle is not consent to something invisible.

### Backend Incarnations

An **incarnation** is an opaque agent-native session ID. Sylph sessions may have multiple incarnations (after lifecycle resets); native sessions have exactly one.

Lifecycle triggers (Sylph only), as designed:
- 24 hours since last turn — *unimplemented*
- Local time crosses 04:00 — *unimplemented*
- User-initiated reset — *unimplemented; no endpoint*

So in practice a session has exactly one incarnation today, whatever its origin, because
nothing creates a second. The table and the columns exist and are correct for the design;
`db/store.rs:518` (`end_incarnation`) is the function a reset would call, and it is
uncalled.

On reset, when built:
1. Discard old `backend_session_id`
2. Create new incarnation
3. Next turn starts fresh agent context

Native sessions skip this entirely — one incarnation, never expires. That part is true
today by construction rather than by policy.

## Turn Execution Model

A **turn** is a durable execution resource:

1. Client calls `POST /api/v1/sessions/{id}/turns` with idempotency key
2. Backend returns `202 Accepted` with turn ID immediately
3. `TurnSupervisor` establishes a link if the session is detached, then sends the
   turn as a `send` frame on it
4. Client streams events via `GET /api/v1/turns/{id}/events` (SSE)
5. Turn completes (or fails); status recorded when the `turn` event lands

**Properties:**
- Turn creation is idempotent (via client-provided key)
- HTTP request ≠ turn lifecycle (turn survives request failure)
- Browser can reconnect to SSE stream using `Last-Event-ID`
- Service restart marks in-flight turns as `interrupted`

### Process Supervision

**A turn has no process of its own.** This is the largest change v3 made to this
subsystem, and the reason the section below is short. The v1 supervisor spawned a
process per turn and read its exit code to learn the outcome; under `sylph.agent.v3`
the adapter is a long-lived **link** and a turn is a message on it, so the supervisor
no longer *drives* a turn to completion — it marks running, sends, and waits to be
told. The link's drain task journals the events. `backend/src/supervisor/turn.rs`
states this at the top of the module, and `docs/session-protocol.md` § The unit is a
session, not a turn is the reasoning.

Process supervision is therefore a property of the **link**, not the turn.
`LinkManager` owns it:

- Spawns the adapter in a new process group, so a stop signal reaches the CLI the
  adapter exec'd rather than only the wrapper script
- Reads NDJSON from its stdout incrementally, one frame per line
- Forwards SIGTERM on teardown, escalating to SIGKILL after a grace period
- Tears down links idle for 30 minutes (`IDLE_TIMEOUT`), which is what keeps a live
  process from outliving its usefulness — history is unaffected and the next turn
  re-establishes

Concurrency: one turn per session at a time; a second is refused rather than queued
(`SESSION_ALREADY_RUNNING`), because the link holds one conversation.

## Storage Architecture

### Journal (journal.db)

The hot tier. SQLite with WAL mode and `synchronous = NORMAL`, holding:
- Sessions (all sessions created/imported by WebApp)
- Backend incarnations (lifecycle tracking)
- Turns (execution records)
- Events for turns that have not settled yet, including deltas

Bounded by consolidation rather than by a retention window: when a turn reaches a
terminal state its events move into `history.db` and its journal rows are deleted.
The file therefore stays small, which is the point of the split — see
`docs/storage.md`.

Ephemeral events (deltas) are batched into one transaction per ~50 ms window;
stable events commit immediately, because a reconnect replays stable events from
the journal and ephemeral ones from the hub's in-memory ring.

### History (history.db) and CAS

The warm and cold tiers. `synchronous = FULL`, because it is the record. Holds one
row per block with its deltas folded into packed offset and timing arrays, plus
content-addressed blobs in `blobs/` for payloads of 4 KB or more. Grows without bound and
is read-mostly; pruned only by watermark.

### Archive

TraceDB is an embedded crate, not a service: it is a dependency of this binary, with
no daemon, no `PATH` lookup, and no JSON round trip. What survives from the
subprocess era is the *rule* — go through the crate's API, never its SQL — because
the schema is the crate's to change and reading it directly makes every Sylph query
a hidden coupling. What does not survive is the mechanism the rule used to name.

Its value is `parsers/` — five vendors' native formats, and the reason not to
reimplement. Operations Sylph calls: `list` (filtered session listing, carrying
lineage and subagent counts), `show` (one session's trajectory), `coverage` (one
session's ingest bounds), `search` (FTS5 across agents).

Sylph reads the archive for the session list and archive browser, for native session
import, and for cross-agent search. Search is FTS5 work and stays ~200 ms while
every other operation is microseconds to sub-millisecond, so it is the one read that
must never go per-row.

Sylph owns ingestion; nothing outside this binary writes the archive. Ingestion is
**one strategy per agent, not one scan**, and a shared timer must not assume
otherwise: nothing constrains an agent to write a per-session file, or to produce the
fingerprint the ingest path requires.

The archive is fed by *parsing native sources*, never by Sylph handing it normalized
events. That direction is the rule; it is not the claim that Sylph-born sessions are
absent from the archive, since a vendor CLI writes its own file and the ordinary
parser path ingests it — which is what retention confirms a watermark against. See
`data-architecture.md` § Sylph's normalized stream does not enter the archive for the
three structural reasons, and for what the boundary leaves unbuilt.

### Retention

Watermark-driven, never time-driven. A retention pass runs at startup and every
five minutes, takes a bounded batch of sessions holding blocks (least-recently
checked first), and asks one question per session: what sequence number has a
downstream authority confirmed?

Sessions split into two classes, and collapsing them would destroy the only copy
of the second:

| Class | When | Rule |
| --- | --- | --- |
| Delegated | The environment keeps an agent-native store TraceDB ingests | Prune below the confirmed seq |
| Terminal | No ingestion path — a sandbox, or a session that never linked | Compact, never prune |

The delegated watermark is derived from TraceDB by archive id
(`<agent>:<native-id>`, a different id space from Sylph's session id), bounded by
`endedAtMs` and admitted only for a `full` record. `docs/storage.md` governs the
whole design; `/api/v1/diagnostics` reports each pass.

## Event Streaming

### NDJSON Protocol

Runners output normalized events to stdout:

```json
{"kind":"thinking","phase":"started","seq":0}
{"kind":"thinking","phase":"delta","seq":1,"text":"Analyzing"}
{"kind":"thinking","phase":"completed","seq":2,"text":"Analyzing the codebase"}
{"kind":"assistant","phase":"delta","seq":3,"text":"I'll"}
{"kind":"assistant","phase":"completed","seq":4,"text":"I'll implement..."}
{"kind":"tool_call","phase":"started","seq":5,"name":"Write","streamId":"tc_1"}
{"kind":"tool_call","phase":"completed","seq":6,"name":"Write","text":"..."}
{"kind":"usage","seq":7,"data":{"inputTokens":1234,"outputTokens":567}}
```

**Event types:**
- `thinking` — reasoning/scratchpad
- `assistant` — final assistant text
- `tool_call` / `tool_result` — function calls
- `usage` — token counts
- `status` / `error` — system messages

**Phases:**
- `started` — event begins (ephemeral)
- `delta` — incremental update (ephemeral)
- `completed` — event finalized (stable, persisted)

### SSE Streaming

Backend normalizes NDJSON → SSE:

```
event: thinking
data: {"seq":0,"phase":"started","text":""}

event: thinking
data: {"seq":2,"phase":"completed","text":"Analyzing the codebase"}
```

**Reconnection:**
- Client sends `Last-Event-ID: N` or `?after_seq=N`
- Server replays from journal + live projection
- Stable events always available
- Ephemeral events available if still in live projection (bounded)

## Security Model

**Loopback-only:**
- Default bind: `127.0.0.1:4317`
- Cookie: process-local, HttpOnly, SameSite=Strict
- Host header validation (reject DNS rebinding)
- Origin validation for mutations

**Out of scope until auth redesign:**
- Remote access
- Multi-user
- TLS termination

Exposing beyond loopback requires:
- Authenticated sessions
- CSRF tokens
- Origin allowlists
- Attachment size enforcement
- Explicit permission policy

### Filesystem exposure

Three endpoints turn a client-supplied string into a filesystem operation, and
they are bounded differently because their roots differ. The *check* is shared —
`backend/src/paths.rs` canonicalizes, then compares component-wise with
`Path::starts_with` — because a containment rule implemented twice is one that
will diverge. Canonicalizing first is what defeats both `..` and a symlink whose
escape lives on disk rather than in the request.

| Surface | Root | Scope |
| --- | --- | --- |
| `GET /sessions/{id}/files` | the session's journalled `workdir` | list one directory |
| `GET /sessions/{id}/files/content` | same | text and image bytes, encoding decided by the bytes |
| `GET /sessions/{id}/files/document` | same | parsed structure for Office, CSV, PDF |
| `GET /sessions/{id}/files/raw` | same | raw bytes the browser renders itself |
| `POST /sessions/{id}/files` | same | create files, no overwrite by default |
| `GET /fs/{roots,tree}` | `Config::browse_roots` (`$HOME` + `SYLPH_BROWSE_ROOTS`) | list directories only |

Containment is asserted per route rather than assumed inherited, because a new
route added beside a safe one is exactly where the check goes missing. `/files/raw`
also carries hardened response headers: dropping the CSP header there silently
reintroduces cookie disclosure through previewed HTML, which was measured rather
than assumed, so the headers are asserted server-side.

Two decisions are worth recording, because both could reasonably have gone the
other way:

**Machine-level browsing lists directories, never file contents.** It exists only
so a user can name a workspace, which a browser cannot do for itself. Enumerating
directory names is a strictly smaller disclosure than serving bytes, and there is
deliberately no `/fs/content` — reading bytes stays session-scoped, where the root
is journalled rather than chosen. A symlink leaving a root is omitted from
listings rather than merely refused, so it is never advertised as a node to try.

**Writing is confined to the session's own workdir.** The agent driving a session
already writes anywhere under `workdir`, so a browser upload into the same subtree
grants no new capability; it only makes an existing one explicit. Allowing uploads
anywhere the *picker* can reach would have given the browser a wider write range
than the agent, which is why the upload root is the journalled workdir and not the
browse roots.

**`browse_roots` bounds two things, and the second is the larger one.** It bounds
what `/fs/*` may enumerate, and it bounds what a client may *name* as a session's
`workdir` — `POST /sessions` and `POST /sessions/import` both go through
`contain_workdir`, which derives the stored path from `paths::contain_in_any` rather
than checking one path and using another. A `workdir` is the `current_dir` of a
spawned agent, so the naming bound governs where arbitrary code may run, while
enumeration only governs which names are visible. Widening the setting widens the
first; read `configuration.md` § `browse_roots` bounds where agents run before doing
so. One setting rather than two because `/fs/*` exists solely to name a workdir, so a
second key would have to be kept equal to this one.

## Agent Discovery

Adapters are searched in two roots, bundled first: the `adapters/` directory
shipped beside the binary, then `~/.sylph/agents/`. An agent is a directory holding
an executable `adapter.sh`.

Two words, both current: an **agent** is what a user picks, and an **adapter** is the
`adapter.sh` that speaks for it. A **runner** was v1's process-per-turn script, deleted
in Phase 12; the name no longer appears in a path. It survives only in comments that
describe the v1 contract as history, where it is the right word.

Discovery still recognizes a directory holding `runner.sh` or `runner.py` and no
`adapter.sh` — not for compatibility, but to report it as **broken** rather than skip
it. Such a directory is an agent that cannot be spoken to, not an absent one, and
silently skipping it is what produced the `0 usable, 0 broken` total failure.

The startup log says `Agent discovery: N usable, M broken`, and that string is what to
grep for when a session cannot be created. Bundled first means a user-added adapter of
the same name *shadows* rather than duplicates — the escape hatch when a vendor changes
faster than the shipped adapter catches up. See `docs/configuration.md`.

Discovery is a **live handshake**, not a static descriptor read. The backend
scans both roots on startup, and for each directory holding an `adapter.sh` it
spawns the adapter, exchanges `hello`/`ok`, and keeps what came back — then caches
it in memory, refreshable via the API. `DISCOVERY_TIMEOUT` is 10s, matching the
link's own budget: an adapter that cannot say who it is in ten seconds will not run
a turn either. `docs/session-protocol.md` § Handshake is authoritative for the
frames.

A directory that holds a `runner.sh` or `runner.py` but no `adapter.sh` is recorded
as **broken**, not skipped. Such a directory is an agent this server cannot speak
to rather than an ordinary folder, and skipping it silently is what once made a
stale tree report zero usable *and* zero broken — so nothing anywhere said the
directory had been seen and rejected.

**Descriptor schema** (`AgentDescriptor` in `backend/src/agents/mod.rs`):
```json
{
  "name": "claude",
  "description": "Claude Code",
  "version": "2.1.238",
  "session_key": "…",
  "flags": {
    "model": {"choices": ["opus-5", "sonnet-5"], "description": "…"},
    "effort": {"choices": ["low", "medium", "high", "xhigh", "max"]}
  },
  "commands": {},
  "supports": ["interrupt", "steer", "approval", "subagent", "slash"]
}
```

The shape is unchanged from the v1 descriptor so no client had to change: `flags`
and `commands` are still maps keyed by name, not arrays. `version` is the agent
CLI's own, learned from the handshake rather than declared by the wrapper.

`flags` is the **spawn-time** option schema: what can be chosen before a session
exists, which is all the create-a-session screen can know. A flag with an empty
`choices` list is free-form; an enumerated one is validated against its own
enumeration when `PATCH /sessions/{id}` writes it. Nothing in the UI hardcodes a
model list, which is why `codex` correctly shows effort with no model selector.
Anything adjustable on a *live* session is a config facet instead
(`docs/session-protocol.md` § Session config).

`commands` here is usually **empty, and deliberately so**. A project's real command
list depends on files the CLI reads at startup, so it belongs to a session rather
than to an agent: `GET /sessions/{id}/capabilities` is where dozens of them come
from. The field stays in the shape so the settings UI still renders.

`supports` is a closed vocabulary that drives the UI directly — a composer must not
offer interrupt to an adapter that cannot interrupt.

**A turn is not an invocation.** Under v3 the adapter is a long-lived process and
a turn is a message on its link, so there is no per-turn command line: the session
is opened once with `open`, and each turn is a `send` frame on the same stdin.
`docs/session-protocol.md` is authoritative for both. What the process gets at
spawn time is the environment; what it gets per turn is a frame.

Identity is still enforced at that seam, and still through `SYLPH_SOUL`. Injection
is the adapter's job, but *whether* it happens is the backend's: a native session
sends the empty string, which the protocol reserves for "inject nothing". An unset
variable means "resolve your default", which is not the same thing — and a
configured path that does not exist resolves to none, matching the adapters' own
`[[ -f "$SOUL_PATH" ]] || SOUL_PATH=""`.

The child runs in a new process group (`process_group(0)`), so a stop signal reaches
the CLI the adapter exec'd rather than only the wrapper script. Adapters stream
NDJSON to stdout and the backend consumes it incrementally — one frame per line,
which is what lets a turn be observed while it runs rather than after it ends.

There is no degraded path for v1: `runner.sh` is not discovered, `SYLPH_STREAM_FORMAT`
has no meaning, and a directory without `adapter.sh` is not an agent. The reasoning
for refusing a compatibility shim is in `docs/session-protocol.md` § "v1 is not
supported".

## Data Flow

### Create Sylph Session

1. Client: names a directory — from a sidebar group that already is one, from the
   start screen, or through the `/fs/*` picker. There is no form on this path
2. Client: `POST /api/v1/sessions`
3. Backend: generate UUID, insert into `sessions` table
4. Backend: return `201` with session object
5. Frontend: navigate to `/sessions/{id}`

The agent is switchable until the first turn, because `agent` is fixed at creation
server-side; switching on an empty session creates a new one, which is correct
rather than a workaround since there is nothing yet to preserve. A draft nobody
sends anything in is removed by `DELETE /api/v1/sessions/{id}`, which refuses any
session carrying a turn.

### Send Message

1. Client: `POST /api/v1/sessions/{id}/turns` with idempotency key
2. Backend: check idempotency, insert turn (status=queued)
3. Backend: return `202` with turn ID
4. Client: open SSE stream `GET /api/v1/turns/{id}/events`
5. TurnSupervisor: establish a link if detached, `send` the turn, set status=running
6. Backend: adapter NDJSON → journal → SSE → client
7. The adapter's `turn`/`completed` event arrives: set status=completed. There is no
   process exit to read, which is why the outcome is an event rather than an exit code
8. SSE stream closes

### Import Native Session

1. Client: `POST /api/v1/sessions/import` with TraceDB session ID
2. Backend: query TraceDB for session metadata
3. Backend: create session with origin=native, identity_mode=preserve
4. Backend: return `201`
5. Frontend: navigate to session, show preservation policy banner
6. User sends first message: creates turn linked to imported session

### Search

1. Client: `Cmd+K` opens the search overlay
2. Frontend: ranks live sessions client-side from the shared sessions query, and
   queries the archive via `/api/v1/search?q=...`
3. Backend: forward to TraceDB JSON API
4. Backend/frontend: deduplicate an archived hit that is already imported, by
   `trace_session_id` or by the agent's `<agent>:<native-session-id>` identity
5. Frontend: render live and archived results together, archived marked `⌗`

Browsing by *directory* rather than by query is a different path:
`Cmd+Shift+K` pages `GET /api/v1/archive/sessions` for one workspace, which is what
answers "what happened here" when the user has no search term.

## Deployment Model

**Development:**
- `cargo run` for backend
- `npm run dev` for frontend, with Turbopack reload

**Production:**
- Single Rust binary: `cargo build --release`
- Frontend as `output: "standalone"`, a self-contained bundle carrying its own
  `node_modules`. **Not** a static export: `/sessions/[id]` and `/archive/[...id]`
  are dynamic routes and the proxy rewrite to the backend is a server concern, so
  neither survives one
- LaunchAgent for macOS auto-start
- Health checks on startup
- Logs under the install prefix, rotated at 16 MB keeping one generation

`SYLPH_WEB_BACKEND` is compiled into the route manifest, so it must be set for
`next build` and not only for `next start`. See `docs/installation.md`.

**Sylph home** (`~/.sylph` by default, `SYLPH_HOME` to relocate):
- `config.toml` — settings; `SOUL.md` — the identity preamble
- `data/journal.db` — the hot tier: unsettled turns
- `data/history.db`, `data/blobs/` — consolidated history and its payloads
- `data/attachments/{session}/{turn}/` — staged files
- `agents/` — user-added adapters; `scratch/` — Quick Start's working directory

## Extension Points

Future features should preserve these boundaries:

1. **Runner protocol** — NDJSON events are additive; new kinds/phases OK
2. **API versioning** — there is none, and adding one would be the mistake. The
   `/api/v1/` prefix is a path segment, not a compatibility promise: this backend's
   only client is the frontend in this repo, both are built from one commit, and a
   token that rotates on restart means no third party is holding a session across a
   deploy. So a breaking change edits the endpoint, the schema, and the client in
   place. Standing up `/api/v2/` beside a live `/api/v1/` would double the surface
   under test to serve a caller that does not exist — and, worse, make the old shape
   survivable, so nobody would finish the migration. See `AGENTS.md` § Compatibility
3. **Database migrations** — numbered, transactional, append-only
4. **TraceDB interface** — always via JSON protocol, never direct schema access
5. **Session origin** — immutable policy decision, never silently changed

Planned extensions:
- Diff/plan event types
- Approval requests (interactive prompts from agent)
- Per-turn artifacts (screenshots, logs)
- Session forking and reconstruction
- Permission audit log
