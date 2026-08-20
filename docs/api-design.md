# API Design

Sylph Web exposes a versioned REST API with SSE streaming for real-time updates.

## Design Principles

1. **Versioned endpoints** — `/api/v1/*` for stability; breaking changes increment version
2. **RESTful resources** — sessions, turns, events, agents
3. **202 Accepted for async work** — turn creation returns immediately, client streams events via SSE
4. **Idempotent operations** — turn creation uses idempotency keys
5. **Reconnectable streams** — SSE supports `Last-Event-ID` and `after_seq` for resume
6. **Structured errors** — consistent error shape with code, message, details

## Base URL

`http://127.0.0.1:4317`

All endpoints require authentication (process-local HttpOnly cookie).

## Core Endpoints

### Health & Diagnostics

#### `GET /api/v1/health`

Health check with version information.

**Response 200:**
```json
{
  "ok": true,
  "apiVersion": "1",
  "schemaVersion": 3,
  "tracedb": {
    "available": true,
    "version": "0.5.2"
  }
}
```

#### `GET /api/v1/diagnostics`

Detailed system state (sessions, turns, runners).

**Response 200:**
```json
{
  "sessions": { "total": 15, "idle": 12, "running": 3 },
  "turns": { "queued": 0, "running": 2, "completed": 143 },
  "agents": ["claude", "codex", "pi"],
  "uptime_seconds": 8642
}
```

### Agents

#### `GET /api/v1/agents`

List discovered agents from `--describe`.

**Response 200:**
```json
{
  "agents": [
    {
      "name": "claude",
      "version": "3.1.0",
      "models": ["opus-5", "sonnet-5"],
      "efforts": ["low", "medium", "high"],
      "commands": [
        { "name": "compact", "description": "Compact context" }
      ]
    }
  ],
  "broken": {
    "invalid-runner": "exit code 127: --describe failed"
  }
}
```

#### `POST /api/v1/agents/refresh`

Refresh runner discovery.

**Response 200:** Same as `GET /api/v1/agents`

### Sessions

#### `GET /api/v1/sessions`

List sessions (live + TraceDB, deduplicated).

**Query parameters:**
- `limit` (default 50, max 200)
- `agent` (filter by agent name)
- `origin` (`sylph` or `native`)

**Response 200:**
```json
{
  "sessions": [
    {
      "id": "s_abc123",
      "origin": "sylph",
      "identityMode": "sylph",
      "lifecycleMode": "sylph",
      "agent": "claude",
      "workdir": "/Users/hsw/Workspace/project",
      "title": "Feature implementation",
      "flags": { "model": "opus-5", "effort": "high" },
      "status": "idle",
      "createdAtMs": 1724054400000,
      "updatedAtMs": 1724058000000
    }
  ]
}
```

#### `POST /api/v1/sessions`

Create a Sylph-managed session.

**Request:**
```json
{
  "agent": "claude",
  "workdir": "/Users/hsw/Workspace/project",
  "title": "Feature implementation",
  "flags": {
    "model": "opus-5",
    "effort": "high"
  }
}
```

**Response 201:**
```json
{
  "session": { /* session object */ }
}
```

**Errors:**
- `400` — invalid agent, workdir missing, flags validation
- `409` — session already exists (rare, UUID collision)

#### `POST /api/v1/sessions/import`

Import a native session from TraceDB.

**Request:**
```json
{
  "traceSessionId": "ts_xyz789"
}
```

`traceSessionId` is formatted `<agent>:<native-id>`, exactly as TraceDB
reports it. `title` may be supplied to override the derived one.

Policy-relevant fields are derived from the archive, never taken from the
request: the agent comes from the id prefix, because it selects identity and
lifecycle policy, and the working directory comes from the archived session's
own `system` event. A client naming the wrong agent would otherwise produce a
session governed by the wrong rules. The title falls back to the first line of
the opening user turn.

**Response 201:**
```json
{
  "session": {
    "id": "s_imported",
    "origin": "native",
    "identityMode": "preserve",
    "lifecycleMode": "preserve",
    "agent": "claude",
    "workdir": "/Users/me/project",
    "title": "refactor the store layer",
    "status": "idle"
  }
}
```

`traceSessionId` is deliberately absent from the response: agent-native
identifiers are never exposed in session payloads.

**Errors:**
- `400` — malformed `traceSessionId`, no runner for the originating agent, or
  the archived working directory no longer exists
- `404` — no such archived session (`TRACE_SESSION_NOT_FOUND`)
- `409` — already imported (`SESSION_ALREADY_IMPORTED`)
- `503` — TraceDB unreachable (`TRACEDB_UNAVAILABLE`)

#### `GET /api/v1/sessions/{id}`

Get session detail with incarnations and turn summary.

**Response 200:**
```json
{
  "session": { /* full session */ },
  "incarnations": [
    {
      "id": "i_def456",
      "backendSessionId": "opaque_id",
      "createdAtMs": 1724054400000,
      "lastUsedAtMs": 1724058000000
    }
  ],
  "turns": {
    "total": 5,
    "completed": 4,
    "running": 1
  }
}
```

#### `PATCH /api/v1/sessions/{id}`

Change runner flags or the title of an existing session.

```json
{ "flags": { "model": "opus", "effort": "high" }, "title": "Auth refactor" }
```

Both fields are optional; an omitted field is left untouched. `flags` is a
**complete replacement set**, not a merge — the caller has already validated the
whole set, and merging would let a stale key survive a change the user believed
was total. An empty object clears them.

**Why flags are validated here rather than at turn time.** `POST .../turns` filters
the stored set against the runner's `--describe` schema and silently drops what it
does not recognize. That is right there: a descriptor can change under a session
created before it, and the turn should still run. It is wrong for a user action —
a model chosen in the composer that the runner never declared would be accepted,
stored, dropped at turn time, and the turn would run on the default with nothing
anywhere saying why. Validating on write turns that silent no-op into an error the
UI can show against the control that caused it.

Identity is deliberately not patchable. `agent`, `origin`, `identityMode`, and
`lifecycleMode` decide which runner owns the conversation and whether `SOUL.md`
may be injected; changing them on a session with history would rewrite what that
history was produced by. Native→Sylph conversion is a separate, explicit
operation for the same reason.

Errors: `SESSION_NOT_FOUND` (404), `INVALID_REQUEST` (400) for an undeclared flag
key, a value outside a declared enumeration, or an agent that is no longer
installed.

### Turns

#### `POST /api/v1/sessions/{id}/turns`

Create a turn (send message or attachment).

**Request:**
```json
{
  "text": "Implement feature X",
  "attachments": [
    {
      "name": "screenshot.png",
      "type": "image/png",
      "data": "base64_encoded_content"
    }
  ],
  "idempotencyKey": "client_generated_uuid"
}
```

**Response 202:**
```json
{
  "turn": {
    "id": "t_ghi789",
    "sessionId": "s_abc123",
    "status": "queued",
    "requestText": "Implement feature X",
    "createdAtMs": 1724059200000
  }
}
```

**Errors:**
- `404` — session not found
- `400` — validation (text empty, attachment too large, bad base64)
- `409` — duplicate idempotency key

Limits:
- Max 5 attachments per turn
- Max 8 MiB per attachment
- Max 10 MiB total per turn

#### `GET /api/v1/turns/{id}`

Get turn detail.

**Response 200:**
```json
{
  "turn": {
    "id": "t_ghi789",
    "sessionId": "s_abc123",
    "status": "completed",
    "requestText": "Implement feature X",
    "runnerExitCode": 0,
    "createdAtMs": 1724059200000,
    "startedAtMs": 1724059205000,
    "completedAtMs": 1724059312000
  }
}
```

#### `GET /api/v1/turns/{id}/events`

Stream turn events via SSE.

**Query parameters:**
- `after_seq` (default -1, replay from this sequence number)

**Response 200 (text/event-stream):**
```
event: thinking
data: {"seq":0,"phase":"started","text":""}

event: thinking
data: {"seq":1,"phase":"delta","text":"Analyzing"}

event: thinking
data: {"seq":2,"phase":"completed","text":"Analyzing the codebase"}

event: assistant
data: {"seq":3,"phase":"delta","text":"I'll implement"}

event: assistant
data: {"seq":4,"phase":"completed","text":"I'll implement feature X..."}

event: tool_call
data: {"seq":5,"phase":"started","name":"Write","streamId":"tc_1"}

event: tool_call
data: {"seq":6,"phase":"completed","name":"Write","text":"file_path: src/feature.rs\n..."}

event: usage
data: {"seq":7,"data":{"inputTokens":1234,"outputTokens":567}}
```

SSE reconnection:
- Client sends `Last-Event-ID: {seq}`
- Server replays from `seq+1`
- Stable events replayed from journal
- Ephemeral events replayed from bounded projection

#### `POST /api/v1/turns/{id}/stop`

Stop a running turn.

**Response 202:**
```json
{
  "turn": {
    "id": "t_ghi789",
    "status": "stopping",
    /* ... */
  }
}
```

Process receives SIGTERM, escalates to SIGKILL after 2 seconds.

### Workspaces

#### `GET /api/v1/workspaces`

Sessions rolled up by the directory they run in, for the left sidebar and the
settings panel.

A client could derive this from `GET /sessions`, but two facts are only knowable
server-side: the absolute path of the Quick Start directory, which requires
expanding `~`, and counts spanning more sessions than the `LIMIT`-capped session
listing returns.

**Response 200:**
```json
{
  "quickStartDir": "/Users/you/.agents",
  "dataDir": "/Users/you/.agents/data/sylph/web",
  "home": "/Users/you",
  "workspaces": [
    {
      "path": "/Users/you/Workspace/sylph-web",
      "name": "sylph-web",
      "sessionCount": 8,
      "runningCount": 1,
      "lastUsedMs": 1755600000000,
      "isQuickStart": false
    }
  ]
}
```

Ordered newest-first by `lastUsedMs`. The Quick Start directory is always
present, even with no sessions, because the sidebar section and the settings row
exist before the first quick session is created.

### Files

Browsing a session's working directory, for the Artifacts pane. Reads are
unrestricted within the root; writing is confined to one endpoint
(`POST .../files`) that creates files and refuses to overwrite by default. There
is still deliberately no rename or delete endpoint: the agent mutates the
workspace, the browser adds inputs for it to read.

**Path safety.** These are the only endpoints that turn a client string into a
filesystem read, so containment is enforced on the *canonical* path, never on the
string. The root is the session's `workdir` as recorded in the journal, so no
request can choose its own root; the joined path is canonicalized, which resolves
`..` and every symlink component, and is then required to still be inside the
canonical root. A symlink pointing outside the workspace is refused and is also
omitted from listings. See `backend/src/api/files.rs` for the threat model.

#### `GET /api/v1/sessions/{id}/files?path=`

One directory level. `path` is relative to the workdir; omitted or empty means the
root. Lazy expansion is what keeps a repository with 100k files usable.

**Response 200:**
```json
{
  "root": "/Users/you/Workspace/sylph-web",
  "path": "backend/src",
  "entries": [
    { "path": "backend/src/api", "name": "api", "isDir": true, "modifiedAtMs": 1755600000000 },
    { "path": "backend/src/lib.rs", "name": "lib.rs", "isDir": false, "size": 5120, "modifiedAtMs": 1755600000000 }
  ],
  "truncated": false
}
```

Directories sort before files, then case-insensitively by name. `.git`,
`node_modules`, `target`, `.next`, and similar machine-generated directories are
never listed. `truncated` is true past 2000 entries in one directory.

#### `GET /api/v1/sessions/{id}/files/content?path=`

One file's contents.

**Response 200:**
```json
{
  "path": "README.md",
  "absolutePath": "/Users/you/Workspace/sylph-web/README.md",
  "size": 2048,
  "encoding": "text",
  "mimeType": "text/markdown",
  "content": "# Sylph Web\n...",
  "truncated": false,
  "modifiedAtMs": 1755600000000
}
```

`encoding` is `text`, `image`, or `binary`, and is decided by whether the bytes
decode as UTF-8 — not by the extension. A `.log` holding binary output is
therefore reported as `binary` rather than rendered as mojibake, and a file with
no extension is still served as text when it is text. For `image`, `content` is
base64. For `binary`, `content` is absent.

Text over 1 MiB is truncated to a 256 KiB prefix with `truncated: true`. An image
over 1 MiB is not served at all — a partial image decodes to nothing — and comes
back as `binary` with `truncated: true` so the client can explain why.

Errors: `FILE_NOT_FOUND` (404) for a missing path, `POLICY_VIOLATION` (403) for
one outside the workdir, `INVALID_REQUEST` (400) when a directory is requested as
content or vice versa.

#### `POST /api/v1/sessions/{id}/files`

Write one file into the session's working directory.

```json
{ "path": "docs/assets", "name": "diagram.png", "data": "<base64>", "overwrite": false }
```

`path` is the destination *directory*, relative to the workdir; empty means the
root. `data` is base64 with no `data:` prefix — the same encoding turn attachments
use, so a client has one encoder. `name` is reduced to a single path component, so
a separator in it cannot rebuild a path.

**Response 201:**
```json
{ "path": "docs/assets/diagram.png", "absolutePath": "/Users/you/p/docs/assets/diagram.png", "size": 20480 }
```

**Why writing is allowed here at all.** Reading and writing are not symmetric
risks, so this does not inherit the read endpoints' justification. The argument is
narrower: the agent driving this session already writes anywhere under `workdir`
— that is what a coding agent does — so an upload confined to the same subtree
grants no capability the session did not already have. What it must not become is
a general file-writing service, so the root is the journalled `workdir` and never
a client-supplied directory, the destination goes through the same containment
check as reads, and an existing file is refused unless `overwrite` is explicitly
true. Per-file size shares the 8 MiB attachment ceiling.

Errors: `POLICY_VIOLATION` (403) outside the workdir, `INVALID_REQUEST` (400) for
an existing file without `overwrite`, `ATTACHMENT_INVALID` (400) for malformed
base64 or an oversized payload.

### Machine-level browsing

Choosing a directory to bind as a workspace. Separate from the session file API
because the *root* differs: that one is rooted at a session's `workdir`, this at
directories named in configuration.

**Why it exists.** A workspace is a directory, and a session cannot be created
without one — but nothing in a browser can name a directory on the machine the
backend runs on. `<input type="file">` deliberately hides real paths, `~` cannot be
expanded client-side, and the machine that matters is the backend's, which may not
be the one running the browser. Without these endpoints, binding a new project
directory is impossible from the UI.

**Bound.** This is the only endpoint that lists directories the client names, so
the bound cannot come from the request. It comes from `SYLPH_WEB_BROWSE_ROOTS`
(colon-separated) plus `$HOME`, checked on the canonical path by the same
containment primitive the session file API uses (`backend/src/paths.rs`).

It is deliberately narrower than the session file API in three ways: only
directories are listed, so it cannot be used to discover file names; there is no
content endpoint, so reading bytes stays session-scoped; and a symlink leaving the
root is omitted from listings rather than merely refused on access. Dotfiles *are*
listed, because `~/.agents` is a legitimate workspace the product points at.

#### `GET /api/v1/fs/roots`

```json
{ "roots": [{ "name": "Home", "path": "/Users/you" }], "home": "/Users/you" }
```

`home` lets a client abbreviate paths as `~/…` without guessing. A configured root
that no longer exists is filtered out rather than returned as an unopenable node.

#### `GET /api/v1/fs/tree?path=`

One directory level, subdirectories only. Omitted or empty `path` means the first
configured root, so a client's first request needs no prior knowledge of them.

```json
{
  "path": "/Users/you/Workspace",
  "parent": "/Users/you",
  "entries": [{ "path": "/Users/you/Workspace/sylph-web", "name": "sylph-web", "modifiedAtMs": 1755600000000 }],
  "truncated": false
}
```

`parent` is **absent** when `path` is itself a configured root. The server
withholds it rather than the client deciding where to stop, so a "go up"
affordance can never offer a path the next request would refuse.

Errors: `POLICY_VIOLATION` (403) for a path outside every root, `FILE_NOT_FOUND`
(404) for one that does not exist.

## Error Responses

Every failing response under `/api/v1/` carries this envelope, with no
exceptions. That includes failures raised before any handler runs: a path that
matches no route answers `NOT_FOUND`, and a known path with the wrong method
answers `METHOD_NOT_ALLOWED`.

The invariant matters more than it looks. A bodiless framework 404 is
indistinguishable, at the client, from a handler reporting that a *file* is
missing — so a client showing "Not Found" cannot say whether the file is absent
or the endpoint is. Clients branch on `error.code`, never on the HTTP status
alone, and that is only sound if every error has a code.

```json
{
  "error": {
    "code": "INVALID_REQUEST",
    "message": "Invalid agent name",
    "details": {
      "field": "agent",
      "value": "unknown-agent"
    }
  }
}
```

`details` is optional and omitted when there is nothing structured to add.

The full set of codes is the `ErrorResponse.error.code` enum in
`docs/api-v1.openapi.json`, which is authoritative. The two router-level codes
are worth calling out here because no handler can produce them:

| Code | Status | Meaning |
| --- | --- | --- |
| `NOT_FOUND` | 404 | No route matches this path. The endpoint does not exist in this build — distinct from `SESSION_NOT_FOUND` or `FILE_NOT_FOUND`, which mean the route exists and the named thing does not. |
| `METHOD_NOT_ALLOWED` | 405 | The path exists but not for this method. |

A client that sees `NOT_FOUND` for an endpoint it expects to exist is talking to
an older backend than it was built against, and should say so rather than
blaming the request.

## Authentication

Process-local HttpOnly cookie set on root page load.

Required for all API calls:
- Cookie: `sylph_web_auth={random_token}`
- Origin must match loopback (127.0.0.1, localhost, [::1])
- Host header validated against allowlist

## OpenAPI Specification

Full machine-readable spec: `docs/api-v1.openapi.json`

Generate TypeScript types:
```sh
npx openapi-typescript docs/api-v1.openapi.json -o frontend/src/types/api.ts
```
