# Architecture

Sylph Web is a local-first native UI for coding-agent sessions. It orchestrates multiple agent runtimes (Claude, Codex, Pi, OpenCode) through a unified interface while preserving their native identities and lifecycle policies.

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
    ├─► history.db + cas/        │
    │   The durable record       ├─► Query & Search
    ├─► TraceDB ─────────────────┘
    │   Stable cross-agent history
    │
    └─► Runners (child processes)
        claude, codex, pi, opencode
```

**Components:**
- **Frontend** — Next.js 16 + React 19, handles UI and real-time updates
- **Backend** — Rust + Axum, REST API + SSE streaming, process supervision
- **Journal** — SQLite (journal.db), sessions and turns that have not settled
- **History** — SQLite (history.db) plus `cas/`, the durable replayable record
- **TraceDB** — External durable store, cross-agent searchable history
- **Runners** — Agent binaries discovered via `--describe`, managed as child processes

## Design Principles

1. **Local-first** — All data stored locally, no cloud dependencies
2. **Origin as policy** — Session origin (`sylph` or `native`) determines identity and lifecycle
3. **Runners own storage** — Agents write their native format; TraceDB ingests asynchronously
4. **Bounded journal, durable history** — Unsettled turns in journal.db, consolidated
   into history.db and pruned only when a downstream authority confirms the data
5. **Durable turn supervision** — Turn execution independent of HTTP request lifecycle

## Session Model

### Session Origins

| Origin | Created by | Lifecycle | SOUL.md injection |
|--------|-----------|-----------|-------------------|
| `sylph` | WebApp | 24h idle / 04:00 reset | Per `identityMode`, default yes |
| `native` | CLI/TUI import | No implicit reset | Never |

**Invariants:**
- Origin is immutable after creation
- Native sessions never receive SOUL.md or lifecycle resets
- Sylph sessions follow managed lifecycle (24h idle, 04:00 cutoff)

### Identity is a second axis, not a synonym for origin

`identityMode` is `sylph` (inject `SOUL.md`) or `preserve` (run the agent on its own
identity). It is chosen when a session is created, defaults to `sylph`, and `PATCH`
refuses it — identity describes what a stored transcript was *produced by*, so
changing it on a session with history would rewrite what that history means.

**A Sylph session that declines injection is still a Sylph session.** Origin stays
`sylph`, and managed lifecycle still applies. Collapsing the two axes would make
"run this agent as itself" indistinguishable from an imported session that must
never be reset or compacted — the invariant above. So:

| | `identityMode: sylph` | `identityMode: preserve` |
| --- | --- | --- |
| `origin: sylph` | The default. Injects, resets. | Agent's own identity, still resets. |
| `origin: native` | Never produced. | Every imported session. |

An import sets both `identityMode` and `lifecycleMode` to `preserve`, derived from
the archive rather than the request. `GET /api/v1/soul` serves the text this
governs, so the toggle is not consent to something invisible.

### Backend Incarnations

An **incarnation** is an opaque agent-native session ID. Sylph sessions may have multiple incarnations (after lifecycle resets); native sessions have exactly one.

Lifecycle triggers (Sylph only):
- 24 hours since last turn
- Local time crosses 04:00
- User-initiated reset

On reset:
1. Discard old `backend_session_id`
2. Create new incarnation
3. Next turn starts fresh agent context

Native sessions skip this entirely — one incarnation, never expires.

## Turn Execution Model

A **turn** is a durable execution resource:

1. Client calls `POST /api/v1/sessions/{id}/turns` with idempotency key
2. Backend returns `202 Accepted` with turn ID immediately
3. `TurnSupervisor` spawns runner as detached process
4. Client streams events via `GET /api/v1/turns/{id}/events` (SSE)
5. Turn completes (or fails); exit code and status recorded

**Properties:**
- Turn creation is idempotent (via client-provided key)
- HTTP request ≠ turn lifecycle (turn survives request failure)
- Browser can reconnect to SSE stream using `Last-Event-ID`
- Service restart marks in-flight turns as `interrupted`

### Process Supervision

`TurnSupervisor` manages runner child processes:

- Spawns in new process group
- Streams stdout/stderr (NDJSON protocol)
- Forwards SIGTERM on stop request
- Escalates to SIGKILL after 2 seconds
- Reaps zombies on exit

Concurrency: one turn per session at a time; queued turns wait.

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
content-addressed blobs in `cas/` for payloads over 4 KB. Grows without bound and
is read-mostly; pruned only by watermark.

### TraceDB

Accessed only through JSON protocol, never direct SQLite queries.

Operations:
- `list` — list sessions with filters
- `show` — fetch session trajectory
- `search` — full-text search

Sylph Web reads from TraceDB for:
- Session list and archive browser (deduplicated by imported TraceDB id or by
  the runner's native session id once ingestion is observed)
- Native session import (creates lightweight continuation)
- Cross-agent search

TraceDB writes come from agent native stores (ingestion is external).

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
| `GET /fs/{roots,tree}` | `Config::browse_roots` (`$HOME` + `SYLPH_WEB_BROWSE_ROOTS`) | list directories only |

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

Note that `POST /sessions` already accepts any `workdir` that exists, with no
containment check. Browsing therefore adds *enumeration*, not the ability to name
a path — a distinction worth keeping in mind before widening `browse_roots`, since
enumeration is the part that scales with an attacker's ignorance of the layout.

## Runner Discovery

Adapters are searched in two roots, bundled first: the `adapters/` directory
shipped beside the binary, then `~/.sylph/agents/`. An agent is a directory holding
an executable `adapter.sh`. Bundled first means a user-added adapter of the same
name *shadows* rather than duplicates — the escape hatch when a vendor changes
faster than the shipped adapter catches up. See `docs/configuration.md`.

Discovery:
1. Backend scans both roots on startup
2. Calls `runner.sh --describe` for each
3. Parses JSON capability descriptor
4. Caches in memory (refresh via API)

**Descriptor schema** (`AgentSpec` in `backend/src/models/agent.rs`):
```json
{
  "name": "claude",
  "version": "3.1.0",
  "description": "Claude Code",
  "flags": {
    "model": {"choices": ["opus-5", "sonnet-5"], "default": "opus-5"},
    "effort": {"choices": ["low", "medium", "high"], "default": "medium"}
  },
  "commands": {
    "compact": {"description": "Compact context", "args": {}}
  }
}
```

`flags` and `commands` are maps keyed by name, not arrays. A flag with an empty
`choices` list is free-form; an enumerated one is validated against its own
enumeration when `PATCH /sessions/{id}` writes it. This is the only source of what
the composer offers, which is why `codex` correctly shows effort with no model
selector — nothing in the UI hardcodes a model list.

Turn invocation:
```sh
SYLPH_STREAM_FORMAT=ndjson-v1 SYLPH_SOUL={path|""} runner.sh \
  --workdir {workdir} \
  --session-id {backend_session_id} \
  --flag model=opus-5 --flag effort=high \
  --attachment {path} \
  --command {name} --arg {key}={value}
# prompt on stdin, closed at EOF
```

The prompt is written to **stdin**, not passed as an argument: a turn's text is
unbounded and would otherwise hit `ARG_MAX`. Flags are passed as repeated
`--flag key=value` pairs rather than as named options, so a new flag an agent
declares needs no backend change.

`SYLPH_SOUL` is where the identity invariant is enforced. Injection is the runner's
job, but *whether* it happens is the backend's: a native session sends the empty
string, which the protocol reserves for "inject nothing". An unset variable means
"resolve your default", which is not the same thing.

The child runs in a new process group, so a stop signal reaches the CLI the runner
exec'd rather than only the wrapper script. Runners stream NDJSON to stdout; the
backend consumes it incrementally.

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
5. TurnSupervisor: spawn runner, set status=running
6. Backend: stream NDJSON → SSE → client
7. Runner exits: set status=completed, record exit code
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
   `trace_session_id` or by the runner's `<agent>:<native-session-id>` identity
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
2. **API versioning** — breaking changes increment `/api/v2/`
3. **Database migrations** — numbered, transactional, append-only
4. **TraceDB interface** — always via JSON protocol, never direct schema access
5. **Session origin** — immutable policy decision, never silently changed

Planned extensions:
- Diff/plan event types
- Approval requests (interactive prompts from agent)
- Per-turn artifacts (screenshots, logs)
- Session forking and reconstruction
- Permission audit log
