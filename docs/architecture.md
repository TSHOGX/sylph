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
    ├─► live.db (SQLite) ────────┐
    │   Active sessions & turns  │
    │                             ├─► Query & Search
    ├─► TraceDB ─────────────────┘
    │   Stable cross-agent history
    │
    └─► Runners (child processes)
        claude, codex, pi, opencode
```

**Components:**
- **Frontend** — Next.js 16 + React 19, handles UI and real-time updates
- **Backend** — Rust + Axum, REST API + SSE streaming, process supervision
- **Live journal** — SQLite (live.db), active sessions and recent events
- **TraceDB** — External durable store, cross-agent searchable history
- **Runners** — Agent binaries discovered via `--describe`, managed as child processes

## Design Principles

1. **Local-first** — All data stored locally, no cloud dependencies
2. **Origin as policy** — Session origin (`sylph` or `native`) determines identity and lifecycle
3. **Runners own storage** — Agents write their native format; TraceDB ingests asynchronously
4. **Bounded live journal** — Recent events in live.db; stable history in TraceDB
5. **Durable turn supervision** — Turn execution independent of HTTP request lifecycle

## Session Model

### Session Origins

| Origin | Created by | Identity | Lifecycle | SOUL.md injection |
|--------|-----------|----------|-----------|-------------------|
| `sylph` | WebApp | Sylph-managed | 24h idle / 04:00 reset | Yes |
| `native` | CLI/TUI import | Preserved | No implicit reset | Never |

**Invariants:**
- Origin is immutable after creation
- Native sessions never receive SOUL.md or lifecycle resets
- Sylph sessions follow managed lifecycle (24h idle, 04:00 cutoff)

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

### Live Journal (live.db)

SQLite database with WAL mode, stores:
- Sessions (all sessions created/imported by WebApp)
- Backend incarnations (lifecycle tracking)
- Turns (execution records)
- Events (stable completed events + bounded live projection)

Events are **not** a second transcript archive. Stable events are pruned after TraceDB ingestion + retention window.

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

### Reconciliation (Future)

Planned reconciliation process:
1. Map live `backend_session_id` to TraceDB session ID
2. Track ingestion watermark
3. Prune duplicated event text from live.db after retention period

Until deployed: live.db grows unbounded (acceptable for development).

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
| `GET /sessions/{id}/files*` | the session's journalled `workdir` | read files and directories |
| `POST /sessions/{id}/files` | same | create files, no overwrite by default |
| `GET /fs/*` | `Config::browse_roots` (`$HOME` + `SYLPH_WEB_BROWSE_ROOTS`) | list directories only |

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

Runners live under `~/.agents/skills/sylph/runners/{name}/runner.sh`.

Discovery:
1. Backend scans runners on startup
2. Calls `runner.sh --describe` for each
3. Parses JSON capability descriptor
4. Caches in memory (refresh via API)

**Descriptor schema:**
```json
{
  "name": "claude",
  "version": "3.1.0",
  "models": ["opus-5", "sonnet-5"],
  "efforts": ["low", "medium", "high"],
  "commands": [
    {"name": "compact", "description": "Compact context"}
  ]
}
```

Turn invocation:
```sh
runner.sh \
  --session-id {backend_session_id} \
  --workdir {workdir} \
  --model {flags.model} \
  --effort {flags.effort} \
  --message {text} \
  --attachment {path}
```

Runners stream NDJSON to stdout; backend consumes incrementally.

## Data Flow

### Create Sylph Session

1. Client: `POST /api/v1/sessions`
2. Backend: generate UUID, insert into `sessions` table
3. Backend: return `201` with session object
4. Frontend: navigate to `/sessions/{id}`

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

1. Client: search input in session rail
2. Frontend: query TraceDB via `/api/v1/search?q=...`
3. Backend: forward to TraceDB JSON API
4. Backend: merge results with live sessions, deduplicate by `trace_session_id`
5. Frontend: render unified session list

## Deployment Model

**Development:**
- `cargo run` for backend
- `npm run dev` for frontend (Phase 2)
- Live reload via cargo-watch and Turbopack

**Production (Phase 5):**
- Single Rust binary: `cargo build --release`
- Static Next.js export (optional, or proxy)
- LaunchAgent for macOS auto-start
- Health checks on startup
- Logging to `~/.agents/data/sylph/web/logs/`

**Data directory:**
- `~/.agents/data/sylph/web/live.db` — SQLite journal
- `~/.agents/data/sylph/web/attachments/{session}/{turn}/` — staged files

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
