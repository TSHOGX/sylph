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

Detailed system state (sessions, turns, runners, retention).

**Response 200:**
```json
{
  "sessions": { "total": 15, "idle": 12, "running": 3 },
  "turns": { "queued": 0, "running": 2, "completed": 143 },
  "agents": ["claude", "codex", "pi"],
  "uptimeSeconds": 8642,
  "retention": {
    "ranAtMs": 1724058000000,
    "examined": 64,
    "confirmed": 12,
    "withheld": 52,
    "terminal": 41,
    "unchanged": 9,
    "blocksPruned": 318,
    "blobsSwept": 4,
    "archiveAvailable": true,
    "bytesHeld": 4183740928,
    "sessionsMeasured": 357,
    "bytesGrewBy": 12043776,
    "largestSessions": [
      {
        "sessionId": "s_abc123",
        "bytes": 1183741952,
        "blocks": 21044,
        "measuredAtMs": 1724058000000,
        "grewBy": 8388608,
        "grewOverMs": 300000
      }
    ]
  }
}
```

Authenticated, unlike `/health`: it carries counts, local filesystem paths, and
runner failure messages.

`retention` is the last completed pass, or `null` before the first one. Two things
about its shape are load-bearing, both covered in `docs/storage.md`:

- **`withheld` beside `terminal`** is what separates correct behaviour from a
  broken pass. A terminal session is *supposed* to keep everything, while a
  delegated session that keeps withholding means the archive is not confirming it —
  and an all-`unchanged` pass means the ingestion daemon has stopped advancing.
- **The byte fields answer "which session is growing".** The counts cannot: forty
  small withheld sessions and one enormous one report identically. `bytesHeld` is
  the *logical* record across every measured session, not the footprint on disk, so
  it exceeds what `du` reports on the data directory — it is a ranking measure, not
  a disk gauge. `grewBy` is signed (negative means pruning worked) and **absent
  when a session has been measured only once**, because one observation is not a
  trend and reporting zero would claim stability.

There is deliberately no configurable size bound. A terminal session's record is
the only copy, so a size-triggered prune is a size-triggered deletion of data
nothing else holds — the same trade a time-based rule makes, refused for the same
reason. `docs/storage.md` § Why no configurable upper bound is offered.

### Identity

#### `GET /api/v1/soul`

The identity text Sylph appends to an agent's prompt, read from disk per request.

This endpoint exists because the toggle that governs injection is meaningless
without it. `SOUL.md` lives in the Sylph home directory — outside any other surface
the app exposes — so asking a user to decide whether to append it to every session,
with no way to read what it says, is asking for consent to something invisible.

**Response 200 — a file resolved and was readable:**
```json
{
  "available": true,
  "path": "/Users/you/.sylph/SOUL.md",
  "content": "# Sylph\n\nYou are ...",
  "bytes": 3184,
  "modifiedMs": 1724058000000
}
```

**Response 200 — no `SOUL.md` on this install:**
```json
{ "available": false }
```

| Field | Present when | Meaning |
| --- | --- | --- |
| `available` | always | A readable `SOUL.md` resolved |
| `path` | `available` | Absolute path, so the user can go edit it |
| `content` | resolved and read | The text as the agent receives it |
| `bytes` | `available` | Size on disk, before any truncation |
| `modifiedMs` | `available` | mtime, epoch milliseconds |
| `truncated` | only when `true` | `content` holds just the first 128 KiB |
| `error` | resolved but unreadable | Why, e.g. `Could not read SOUL.md: PermissionDenied` |

Four decisions this shape encodes:

- **Read per request, not cached at startup.** `SOUL.md` is hand-edited between
  sessions. A cached copy rendered beside a toggle that claims to control the live
  file would be a lie, and the file is a few kilobytes from the page cache — the
  cost of being correct is not measurable.
- **A missing file is `200 {"available": false}`, not `404`.** An install without
  `SOUL.md` is ordinary and runs every session on the agent's own identity whatever
  the client asks for. The UI has to *say* that plainly rather than offer a choice
  over a file that does not exist, and a `404` only reports that a request failed.
- **Unreadable is a separate `error` field, not a `5xx`.** "There is no `SOUL.md`"
  and "there is one and it cannot be read" are different problems with different
  fixes, and the settings panel must render the toggle either way.
- **Truncated at 128 KiB.** A prompt preamble is prose and the real one is ~3 KiB.
  The bound keeps the response a preview: a file that grew to megabytes is a mistake
  worth seeing the head of, not worth shipping to a browser in one string. It cuts
  on a `char` boundary, since slicing raw bytes would render the tail as U+FFFD —
  visible corruption in something meant to be faithful.

Authenticated, like every route but `/health`: it returns an absolute filesystem
path and the full text of a personal file.

Resolution lives in `Config` (`backend/src/config/`), which resolves `paths.soul`
once — `~/.sylph/SOUL.md` by default, overridden by `SYLPH_SOUL`. An explicit empty
`SYLPH_SOUL` yields no identity, matching the adapter protocol's spelling, and a
configured path that does not exist resolves to none for the same reason. Resolving
it in two places is exactly how the settings panel and the spawned adapter would
come to disagree about whether injection is active.

#### `GET /api/v1/config`

What configuration resolved to, and which source supplied each value. The same
report `sylph-web-backend doctor` renders, so the terminal and the settings panel
cannot disagree.

**Response 200:**
```json
{
  "home": "/Users/you/.sylph",
  "configPath": "/Users/you/.sylph/config.toml",
  "configPresent": true,
  "entries": [
    {
      "key": "paths.data_dir",
      "value": "/Users/you/.sylph/data",
      "source": "default",
      "envVar": "SYLPH_DATA_DIR",
      "exists": true
    },
    {
      "key": "server.port",
      "value": "4400",
      "source": "env",
      "envVar": "SYLPH_PORT"
    }
  ],
  "bundledAgentCandidates": [
    { "path": "/opt/sylph/adapters", "exists": true, "chosen": true }
  ]
}
```

Four decisions:

- **`source` and `envVar` on every entry.** A resolved value alone cannot
  distinguish "this path is wrong" from "the path I set is not being read", and
  those need opposite fixes. Naming the variable in the row means acting on it does
  not require consulting a table elsewhere.
- **`exists` is absent, not `false`, for anything that is not a path.** A log level
  rendered as missing reports a working setting as broken. `tracedb.bin` is checked
  against `PATH` when it is a bare command name, since that is the default and a
  filesystem check would call a working install absent.
- **Read-only, with no `PUT`.** `paths.browse_roots` bounds what any client of this
  backend may enumerate and `paths.agents_dir` names directories the server executes
  programs from. A loopback HTTP surface that could rewrite either is a materially
  larger threat-model question than one that displays them, and the file is the
  durable, reviewable record. The panel shows the values and the path to open.
- **Authenticated.** Every value is an absolute local filesystem path. `/health`
  stays unauthenticated for supervisor probes and reports booleans only.

Built per request rather than cached: every `exists` field is a filesystem fact, and
a cached report would keep telling a user a directory is missing after they created
it. It is a handful of `stat` calls against the page cache.

`bundledAgentCandidates` lists every directory bundled-adapter resolution tried, in
order, with the one it took marked. Without it "no agents" is unactionable — a
missing install and a mistyped path produce the same empty list.

See `docs/configuration.md` for the settings themselves.

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
      "nativeSessionId": "native-agent-id",
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
  "identityMode": "sylph",
  "flags": {
    "model": "opus-5",
    "effort": "high"
  }
}
```

`identityMode` is `sylph` (append `SOUL.md`) or `preserve` (run the agent on its
own identity, no injection). It defaults to `sylph`, so a client that omits it
keeps the original behaviour. A closed enumeration rather than a free string: a
misspelling is a `422` naming the field, instead of a journal row carrying a value
nothing downstream matches — which would read as "not sylph" everywhere and
silently disable injection.

**`identityMode` and `origin` are separate axes.** Opting out of injection does
*not* make a session `native`. The WebApp created it, so Sylph lifecycle policy
still applies and it may be reset or compacted; `origin` stays `sylph`. Collapsing
the two would make "run this agent as itself" indistinguishable from an imported
session that must never be reset. See `GET /api/v1/soul` for the text this governs.

**Response 201:**
```json
{
  "session": { /* session object */ }
}
```

**`workdir` is bounded, and the bound is configuration.** It must resolve inside
`paths.browse_roots` or the Quick Start directory, checked on the canonical path so
`..` and symlinks are resolved rather than pattern-matched. This is the same root
set `GET /api/v1/fs/*` browses, deliberately: that endpoint exists to name a
workdir, so a second bound would be a second thing to keep equal.

The reason it is bounded at all is that this field is the `current_dir` of a
spawned agent process, and a coding agent reads and writes the tree it is pointed
at — so it decides where arbitrary code runs. Existence, the only property it was
checked for previously, says nothing about whether it should. See
`docs/configuration.md` § `browse_roots` bounds where agents run.

The **canonical** path is journalled, not the string as sent, so two spellings of
one directory do not become two workspaces and the recorded path carries no
symlinked component that could be re-pointed after the check.

**Errors:**
- `400` — invalid agent, workdir missing or not a directory, flags validation
- `403` — `POLICY_VIOLATION`: the workdir exists but is outside the permitted
  roots. Distinct from `400` on purpose: the request is well-formed and the fix is
  in `config.toml`, not in the request. The message lists what is permitted.
- `409` — session already exists (rare, UUID collision)
- `422` — `identityMode` outside the enumeration

A session already recorded outside the permitted roots stays fully readable:
nothing re-checks a journalled workdir on read, list, or resume. Narrowing
configuration must not orphan history.

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

Live sessions expose `nativeSessionId` after the first runner turn. It is the
agent-native id returned by the runner, and is the id users should copy into
the originating agent or TraceDB. `id` remains the Sylph Web route key. Native
imports also expose `traceSessionId`, the complete TraceDB id. The backend uses
both forms when reconciling a live row with an archive row.

**The archived working directory is bounded exactly like a created one**, and this
is not belt-and-braces. The archive holds every session any agent ever ran on this
machine, so an unbounded import is a *general* way to start an agent anywhere: find
a row that ran in the target directory, adopt it, send a turn. That the path came
from the archive rather than from the request body changes who chose it, not what
happens next — resuming spawns a process there either way.

The consequence is that an archived session which ran outside
`paths.browse_roots` cannot be imported until that directory is permitted. The
error says so, and says what importing would enable, so the refusal reads as a
policy decision rather than as a broken archive.

**Errors:**
- `400` — malformed `traceSessionId`, no runner for the originating agent, or
  the archived working directory no longer exists
- `403` — `POLICY_VIOLATION`: the archived working directory exists but is outside
  the directories this install permits sessions to run in
- `404` — no such archived session (`TRACE_SESSION_NOT_FOUND`)
- `409` — already imported (`SESSION_ALREADY_IMPORTED`)
- `422` — `TRACE_SESSION_NOT_RESUMABLE`: the archive recorded no working directory
- `503` — TraceDB unreachable (`TRACEDB_UNAVAILABLE`)

Those five are deliberately distinct because they have five different fixes:
nothing to do, widen configuration, wrong id, already done, and reinstall the
agent. Collapsing any pair would send a reader to the wrong layer.

#### `GET /api/v1/sessions/{id}`

Get session detail with incarnations and turn summary.

**Response 200:**
```json
{
  "session": { /* full session */ },
  "incarnations": [
    {
      "id": "i_def456",
      "nativeSessionId": "opaque_id",
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

#### `DELETE /api/v1/sessions/{id}`

Discard an empty draft. Returns `204 No Content`.

```sql
DELETE FROM sessions WHERE id = ? AND origin = 'sylph'
  AND NOT EXISTS (SELECT 1 FROM turns WHERE turns.session_id = sessions.id)
```

The two conditions are the whole contract, and they are enforced in the statement
rather than by a read-then-delete: a session that has run a turn, and any session
imported from the archive, are both untouchable through this route. It exists for
one narrow case — a user picks a directory, gets a session, and navigates away
without sending anything. Creation is a mutation now rather than a form, so drafts
are a normal consequence of browsing, and leaving them behind would fill the
sidebar with rows representing nothing.

**Idempotent, and deliberately not 404 on a miss.** A session that does not qualify,
or does not exist at all, still answers `204`. The client's intent is "do not leave
a draft behind", not "assert this row existed" — and the caller is usually an unload
path that cannot act on an error anyway.

There is no route that deletes a session with history. Session delete, move, and
export are all still absent; see the roadmap.

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

**Response headers:**
- `Content-Type: text/event-stream`
- `Cache-Control: no-cache, no-store, no-transform`

`no-transform` (RFC 9111 §5.2.2.6) is required, not decorative. It forbids an
intermediary from re-encoding the body, and without it the Next.js proxy's
`compression` middleware gzips the stream — which cannot be decoded until a block
flushes, so the client receives the entire turn at once instead of incrementally.
Exactly one `cache-control` header is sent; two would leave the applied directive
up to whichever an intermediary reads first.

Note that each frame is **named** after its event kind (`event: thinking`), so a
client must not dispatch on `EventSource.onmessage`, which by spec receives only
frames named `message`.

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

Delegated work carries `span`, naming the trajectory an event happened in; absent
means the root turn. A `subagent` event's `streamId` *is* a trajectory id, so a
nested delegation is one whose `span` names another — which is how a subagent, a
subagent's subagent, and a dynamic workflow's parallel phases all travel in one
flat stream. See `docs/session-protocol.md` § Delegated trajectories.

```
event: subagent
data: {"seq":8,"phase":"started","streamId":"s1","name":"Explore"}

event: tool_call
data: {"seq":9,"phase":"completed","streamId":"t7","span":"s1","name":"Grep"}

event: subagent
data: {"seq":10,"phase":"started","streamId":"s2","span":"s1","name":"Worker"}

event: subagent
data: {"seq":11,"phase":"completed","streamId":"s1"}
```

Unlike other kinds, a `subagent` event is `stable` at every phase: its `started`
phase is where a trajectory's identity, name, and parent live, so dropping it as an
ephemeral projection would leave the journal holding children that point at a parent
it never recorded. A trajectory an adapter never closes is closed by the server when
the turn terminates, carrying `data.closedBy = "server"` — which a client reports as
*abandoned* rather than *failed*, because nobody said it failed, only that nobody
said.

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

### Requests

The inversion: the agent asks, and a human answers. A request is a resource rather
than a stream message, because the agent blocks until it is answered — so it has to
outlive every client and be answerable from a frontend that attached an hour later.
`docs/session-protocol.md` § Requests the adapter sends governs the wire shape.

#### `GET /api/v1/sessions/{id}/requests`

What this session is waiting on, oldest first — read from the journal rather than
from the link, so it answers after a reconnect, from a different frontend, and for a
session whose link has since died. That last case is exactly when a user needs to
know why nothing is happening.

**Response 200:**
```json
[
  {
    "id": "req_abc123",
    "sessionId": "s_def456",
    "turnId": "t_ghi789",
    "kind": "choice",
    "tool": "AskUserQuestion",
    "title": "Two questions",
    "detailJson": "{}",
    "optionsJson": "[{\"id\":\"submit\",\"label\":\"Send answer\"},{\"id\":\"deny\",\"label\":\"Don't answer\"}]",
    "formJson": "{\"fields\":[{\"type\":\"select\",\"id\":\"q:0\",\"mode\":\"one\",\"choices\":[{\"value\":\"Tabs\"},{\"value\":\"Spaces\"}],\"allowOther\":true,\"notes\":{}}]}",
    "defaultOption": "deny",
    "risk": "safe",
    "allowPersistent": true,
    "status": "pending",
    "createdAtMs": 1730000000000
  }
]
```

`formJson` is the input the request declared it wants collected. Absent means an
approval, whose subject is the one proposed tool input in `detailJson` — see
§ Request forms in the protocol doc for why it is declared rather than inferred.

The adapter's own request id is never serialized. A client that could send it could
answer a request that was never raised.

#### `POST /api/v1/requests/{id}/resolve`

Answer one. Addressed by request id at the top level rather than under a session,
because the id is what the answer is *about*.

```json
{
  "option": "submit",
  "input": {
    "values": {"q:0": "Tabs", "q:1": ["ESLint", "Clippy"]},
    "notes": {"q:0": "only for generated files"}
  }
}
```

For a request with no form, `input` is instead an edited version of what the agent
proposed — how "allow, but change the command" works. Absent means "as proposed".

**Response 200** carries the resolved request.

| Status | Code | When |
| --- | --- | --- |
| 400 | `INVALID_REQUEST` | An option the agent never offered, or an answer that does not fit the declared form |
| 404 | `REQUEST_NOT_FOUND` | No such request |
| 409 | `REQUEST_ALREADY_RESOLVED` | Someone else answered first |
| 409 | `LINK_GONE` | The answer was recorded, but no live agent is waiting for it |

Both validations exist for the same reason: an unrecognized value reaches the vendor
CLI, and what happens there is undefined — for an approval, "undefined" must never be
able to mean "allow". Validation happens *before* the row is claimed, so a refused
answer leaves the request open and answerable rather than consuming it.

The claim is a single `UPDATE ... WHERE status = 'pending'`, so two clients answering
at once produce one winner; the answer reaches the adapter only after the claim
succeeds, which is what stops the loser also reaching it. `LINK_GONE` is a real
outcome rather than an error to hide: the user did answer, and saying nothing is
waiting beats a success that went nowhere.

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
  "quickStartDir": "/Users/you/.sylph/scratch",
  "dataDir": "/Users/you/.sylph/data",
  "home": "/Users/you",
  "workspaces": [
    {
      "path": "/Users/you/Workspace/sylph-web",
      "name": "sylph-web",
      "sessionCount": 8,
      "runningCount": 1,
      "lastUsedMs": 1755600000000,
      "isQuickStart": false,
      "archivedCount": 0
    }
  ]
}
```

Ordered newest-first by `lastUsedMs`. The Quick Start directory is always
present, even with no sessions, because the sidebar section and the settings row
exist before the first quick session is created.

**Query parameters.** `includeArchived=true` also reports directories that exist
only in the TraceDB archive, with `archivedCount` set. It is off by default
because the sidebar polls this endpoint and needs only live sessions to group,
while an archive scan walks every archived session. Surfaces where the user
*picks* a directory pass it: a project someone is about to work in has no live
session yet, so without this it could never be pinned or selected. When the flag
is absent `archivedCount` is `0`, which therefore means "not counted" rather than
"no history". An unreachable archive degrades to live-only rather than failing the
request.

### Archive

Read-only browsing of durable coding-agent history, by directory.

`/search` answers "which archived session mentioned X", which only helps someone
who already knows what to ask. These endpoints answer what the sidebar asks:
"what happened in this directory before?" Without them a freshly pinned workspace
renders as empty while the archive holds hundreds of sessions for that same path.

**Reading is not importing.** `POST /sessions/import` adopts a session so it can
be *continued*: it writes a row, requires a runner for the originating agent, and
requires the recorded `cwd` to still exist. None of that is needed to read a
transcript, and all of it fails for exactly the old sessions a user most wants to
look back at. So these endpoints mutate nothing, and import stays an explicit
action taken from the archived view.

#### `GET /api/v1/archive/sessions`

| Parameter | Required | Meaning |
| --- | --- | --- |
| `workdir` | yes | Absolute directory to list history for |
| `limit` | no | Rows to return, 1–100, default 20 |
| `cursor` | no | `nextCursor` from a previous response |

**Response 200:**
```json
{
  "sessions": [
    {
      "id": "claude:f1830315-5ce1-44e3-86c0-8f65ccbc961e",
      "agent": "claude",
      "label": "Fix the SSE reconnect loop",
      "cwd": "/Users/you/Workspace/sylph-web",
      "model": "claude-opus-5",
      "startedAtMs": 1787223579574,
      "endedAtMs": 1787227503012,
      "events": 419,
      "subagentCount": 2,
      "sessionId": "0c3f..."
    }
  ],
  "nextCursor": "1787223880382:636c...",
  "available": true
}
```

`id` is a TraceDB session id, not a Sylph Web session id. `sessionId` is present
only when this archived session has already been imported, which lets the UI open
the live conversation instead of listing one conversation twice.

Three filters are applied here rather than upstream, each for a measured reason:

- **Exact directory.** TraceDB matches `cwd` as a substring
  (`cwd LIKE '%' || ? || '%'`), so a request for `/w/app` also returns
  `/w/app-old` and every subdirectory. Rows are re-filtered for exact equality
  after normalizing trailing slashes.
- **Lineage rollup.** TraceDB's `list` does not collapse fork lineage, though
  `search` does, and delegated sessions are 51% of a real archive (2238 of 4416
  non-empty). They fold into the session that spawned them as `subagentCount`,
  since listing them as peers buries the sessions a user actually started. See
  **Lineage** below, which is the subtlest part of this endpoint.
- **Empty sessions.** Rows with `events: 0` are dropped; they are common for
  aborted runs and there is nothing to read.

Because those filters shrink a page, one request may walk several upstream pages
to fill `limit`, bounded by rows examined rather than pages. That bound is
generous on purpose: the substring `cwd` match means a parent directory's pattern
also matches everything beneath it, and those sort by recency rather than depth,
so an exact match can sit far into the scan. One real directory with 88 sessions
of its own had its first exact match ~1300 rows in, and a page-count cap reported
it as empty. `nextCursor` continues from wherever the scan stopped.

##### Lineage

The archive records two different relationships, and they want opposite
treatment. Neither is reported by `list`, so each candidate row costs one cheap
metadata `show` (~0.5 ms); the `/agent-` id suffix is only a fallback for when
that lookup fails, because it misses the 199 sessions that record a parent without
carrying a suffix.

- **`parent_session_id` — delegated work.** Folded into the parent as
  `subagentCount`, *provided the parent is in the directory being listed*. 484 of
  2238 subagents ran somewhere else — a worktree or a subdirectory — and folding
  those into an absent parent deletes them from the only listing that would show
  them, which emptied 42 directories, one holding 193 sessions. A cross-directory
  child is therefore promoted to a row of its own: in its own directory it is not
  delegated work, it is the work.
- **`forked_from` — the same conversation resumed.** A fork replays its origin's
  entire prefix, so both sides share every early user turn and read as one
  conversation; listing both shows it twice under one label. One side survives,
  and which one cannot be fixed in advance — of the two real local cases, one has
  its later work in the fork (1108 events against 389) and the other in the origin
  (2459 against 649). Since `list` is ordered by `endedAtMs` descending, the
  survivor is whichever is reached first.

`forkOf` names the suppressed origin on a surviving fork row. Within one response
the loser is already dropped, but a pair can straddle two pages — one measured
pair sits 268 rows apart — and the server keeps no state between requests. A
client that accumulates pages must therefore drop a row whose `forkOf` names a
session it has already shown, and equally a row that a previously-shown fork
names. `components/history-overlay.tsx` is the only such surface; the sidebar
fetches a single page, where the server has already resolved the pair.

`label` falls back through three sources, in order:

1. **The archive's own title.** `claude` records one for 73% of sessions and
   `opencode` for all of them, while `codex` and `pi` record none — so the
   fallbacks are the common path, not the exception.
2. **The opening *typed* message.** Leading harness-injected envelopes
   (`<permissions instructions>`, `<environment_context>`,
   `<skills_instructions>`, memory files, `Caveat:`) are skipped: they are
   byte-identical across every session of an agent and identify nothing.
3. **An envelope's `summary` attribute.** Some delegated sessions have no typed
   turn at all — every one is a `<teammate-message ... summary="...">` block — but
   that attribute is human-written and describes the task ("Author insects pests
   unit"). XML entities in it are decoded.

Measured over the whole local archive, those three take labelled sessions from
94.3% (title plus typed ask alone) to **98.3%**; across the 138 directories that
hold archived history, 99.5% of listed rows carry one.

An unreachable archive answers **200** with `available: false` and a `detail`,
never 503, matching `/search`: TraceDB is optional locally, and the sidebar calls
this for every expanded workspace.

#### `GET /api/v1/archive/sessions/{traceId}`

One archived transcript, for read-only viewing. `traceId` is matched greedily
because a subagent id contains a slash (`claude:uuid/agent-name`).

`limit` (1–5000, default 2000) bounds the events read; `truncated` reports when it
cut the stream short. The cap matters: the largest session in a real local archive
is 8167 events and 20 MB of JSON.

**Response 200:**
```json
{
  "id": "claude:f1830315-5ce1-44e3-86c0-8f65ccbc961e",
  "agent": "claude",
  "label": "Fix the SSE reconnect loop",
  "cwd": "/Users/you/Workspace/sylph-web",
  "startedAtMs": 1787223579574,
  "events": [{ "seq": 2, "kind": "user", "phase": "completed", "...": "..." }],
  "truncated": false
}
```

`events` are normalized into the same `StreamEvent` shape the live SSE stream and
history endpoint use, so one timeline renderer serves both. Four adaptations:

- Every event is `phase: "completed"` and `stability: "stable"`. Archived history
  is settled, and a `delta` phase would leave the timeline waiting for text that
  never arrives. `source` is `archive`.
- Agent-internal `system` events are dropped. Every observed subtype (`mode`,
  `permission-mode`, `task_started`, `world_state`, `model_change`, …) carries a
  raw JSON configuration blob with no live-protocol equivalent, and one measured
  session held 49 of them in 419 events.
- Tool arguments move from `text` into `data.input`, which is where the client
  reads them. `text` is left in place as the documented fallback for a payload
  that is not JSON.
- A `tool_result` is given the same `streamId` as its `tool_call` so the two
  group. The agents record that link differently — `codex` repeats one `call_id`
  on both, `claude` points the result's `parent_id` at the call's `native_id` —
  and both are mapped onto one id.

A missing session is `404 TRACE_SESSION_NOT_FOUND`; an unreachable archive is
`503 TRACEDB_UNAVAILABLE`. Unlike the listing, a single transcript has no useful
degraded form, so it reports the failure rather than rendering an empty one.

### Files

Browsing and managing a session's working directory, for the Artifacts pane. Reads
are unrestricted within the root. Writes are three narrow endpoints — create a
file (`POST .../files`), create a directory (`POST .../files/directory`), delete
one entry (`DELETE .../files`) — and each refuses its destructive default: an
upload will not overwrite unless asked, a mkdir will not replace, and a delete
will not recurse unless asked and will not touch the root at all.

There is still deliberately no rename and no write-in-place: the agent mutates
file *contents*, the browser manages the files around them.

**Why delete is here at all**, when an earlier version of this document said it
never would be: the pane is the only view of the workspace a user has while a
session runs, and a file browser that cannot remove the file it just showed you is
incomplete in a way that pushes the user to a terminal. What made it acceptable is
that the guards are enforceable server-side and are not merely UI conventions —
see the endpoint below. What it costs is honest: this is the one irreversible
operation in the API, because there is no trash to restore from.

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

#### `GET /api/v1/sessions/{id}/files/document?path=&sheet=&row=&rows=`

A parsed representation of a file that is neither plain text nor an image:
spreadsheets, CSV, Word documents, and PowerPoint decks. Parsing is server-side
so the browser receives structure rather than bytes it would need a megabyte of
JavaScript to interpret.

**Why the server parses.** The npm option (`xlsx`, SheetJS) is frozen at 0.18.5
with two unpatched High CVEs reachable on exactly this "parse a file we did not
write" path, and is 7.5 MB unpacked against a first-load budget with ~4 KB of
headroom. `calamine` and `quick-xml` do the same work in the backend, where the
cost is compile time rather than every page load.

The response is a union discriminated on `kind`. `grid` covers
`.xlsx/.xlsm/.xlsb/.xls/.ods/.csv/.tsv`; `blocks` covers `.docx/.pptx`.

**Response 200** (`kind: "grid"`):
```json
{
  "path": "reports/sales.xlsx",
  "absolutePath": "/Users/you/proj/reports/sales.xlsx",
  "size": 20480,
  "modifiedAtMs": 1755600000000,
  "kind": "grid",
  "grid": {
    "sheets": [{ "name": "Sales", "rows": 5000, "cols": 12 }],
    "sheet": 0,
    "row": 0,
    "rows": [
      [{ "v": "Region", "t": "text" }, { "v": "Q1", "t": "text" }],
      [{ "v": "APAC", "t": "text" }, { "v": "1200", "t": "number", "n": 1200 }]
    ],
    "hasHeaderRow": true,
    "truncated": true
  }
}
```

Each cell carries `v`, the string to display, and `t`, one of `text`, `number`,
`bool`, `date`, `formula`, or `error`. Empty cells are `null` rather than an
object, because sheets are sparse and the difference is most of the payload. `t`
drives alignment — numbers and dates right, text left — and `n` carries the raw
number for a `number` cell so the client need not re-parse a formatted string.

Number formatting and date conversion happen server-side: `v` for a date cell is
already an ISO-8601 string, never an Excel serial. Per-cell currency and percent
formats are **not** reproduced — `calamine` folds the number format into the value
and exposes no per-cell format string, so only date-vs-number survives the trip.

`sheet` selects a worksheet by index (default 0). `row` and `rows` window the
result: `rows` defaults to 200 and caps at 500, and an out-of-range window is
clamped rather than refused, with the response echoing what was actually served.
`sheets[]` always reports every sheet's true dimensions, so a client can say what
it is not showing. Windowing is not cosmetic — a 50 000 × 30 sheet takes ~600 ms
to materialize and would be a ~50 MB response.

**Response 200** (`kind: "blocks"`):
```json
{
  "path": "notes/roadmap.pptx",
  "absolutePath": "/Users/you/proj/notes/roadmap.pptx",
  "size": 41984,
  "modifiedAtMs": 1755600000000,
  "kind": "blocks",
  "blocks": {
    "sections": [
      { "title": "Roadmap", "index": 0, "notes": "Mention the CVE research." }
    ],
    "blocks": [
      { "type": "heading", "level": 1, "text": "Quarterly Report" },
      { "type": "paragraph", "spans": [{ "text": "Revenue grew ", "bold": false },
                                        { "text": "12%", "bold": true }] },
      { "type": "listItem", "level": 0, "ordered": false, "text": "APAC led growth" },
      { "type": "table", "rows": [["Region", "Revenue"], ["APAC", "1200"]] }
    ],
    "truncated": false
  }
}
```

Word and PowerPoint share this shape: a slide is a section whose blocks follow it,
so one renderer serves both. Block `type` is `heading`, `paragraph`, `listItem`,
or `table`.

A structured block list is returned rather than Markdown or HTML. Markdown would
mean escaping document prose containing `*`, `_`, or `|` on the way out and
re-parsing it on the way in — two chances to corrupt text that was never Markdown.
HTML would reintroduce an `innerHTML` path into a frontend whose Markdown renderer
is built specifically to avoid one.

PowerPoint slide order comes from `p:sldIdLst` in `ppt/presentation.xml`, resolved
through `ppt/_rels/presentation.xml.rels`. Sorting slide filenames is wrong:
`slide10.xml` sorts before `slide2.xml`, and a deleted-then-re-added slide breaks
the correspondence entirely. Speaker notes come from the per-slide
`ppt/notesSlides/` part and appear as `sections[].notes`. Shape geometry is not
modelled; reading order is what a preview needs.

**Limits.** A file over 25 MiB is refused before parsing — `MAX_PREVIEW_BYTES`
(1 MiB) is the wrong bound here, since a legitimate spreadsheet is routinely
several MiB. Because an OOXML container is attacker-influenced input, a declared
part over the per-part cap is refused from the zip header before decompressing,
and the read is *also* bounded: a 199 KB archive can declare a 200 MB part, and
the declared figure is itself untrusted.

Errors: `FILE_NOT_FOUND` (404), `POLICY_VIOLATION` (403), and `INVALID_REQUEST`
(400) for a directory, an out-of-range sheet index, an extension this endpoint
does not handle, a file that is too large, or a corrupt, encrypted, or truncated
container. A client that receives `INVALID_REQUEST` for a mislabelled file may
retry once against `/files/content`, which decides text-vs-binary from the bytes.

#### `GET /api/v1/sessions/{id}/files/raw?path=&download=`

The file's bytes, whole: images, HTML, PDF, and any file whose bytes are text.

**This is the route "open in a new tab" uses, and why it serves text.** The pane's
preview is bounded on purpose — 1 MiB read, 256 KiB of text returned — because a
side panel cannot usefully render a 40 MiB log and pushing one through JSON stalls
the pane. But "the preview is truncated" must not mean "you cannot see this file",
so the same file is reachable here in full and rendered by the browser's own
viewer, which streams, finds, zooms, and prints better than anything this pane
would reimplement. Nothing is truncated on this route.

`download=true` forces `Content-Disposition: attachment`. It is a separate
parameter from the type allowlist because the two answer different questions: the
allowlist decides what is *safe to render*, this decides what the user *asked
for*. Without it, Download on a PNG would open a tab showing the image.

This is what the image viewer uses, replacing base64-through-JSON. Base64 costs
33% inflation, a JSON parse before first paint, no progressive decode, and no
HTTP caching, and cannot serve a PDF at all. The route is not a weakening of the
model: it sits behind the same auth middleware and the same canonical-path
containment check as the two endpoints above.

**Response 200:** the raw bytes, with headers that are load-bearing rather than
decorative. Serving workspace HTML from this origin without them lets a script in
an agent-generated file read the `sylph_session` cookie — measured, not
hypothetical.

| Header | Value | Why |
| --- | --- | --- |
| `Content-Type` | allowlisted type, else `text/plain` for text, else `application/octet-stream` | Three tiers, never a guess — see below |
| `X-Content-Type-Options` | `nosniff` | Stops an octet-stream or `text/plain` payload being sniffed into `text/html` |
| `Content-Security-Policy` | `sandbox` | Forces an opaque origin however the response is loaded, so even direct navigation reaches neither DOM nor cookie |
| `Content-Disposition` | `inline` for renderable types, else `attachment`; always with the filename | A type we will not render is a download; without the filename it saves as `raw` |
| `ETag` / `Cache-Control` | mtime+size / `no-cache` | Re-viewing revalidates, so an agent's edit is still noticed |

**The three content-type tiers**, narrowest first:

1. An allowlisted type (`image/png`, `application/pdf`, `text/html`, …) is served
   as itself, because the browser has a real viewer for it.
2. Anything else whose bytes *are text* is served as `text/plain; charset=utf-8` —
   never as its own `text/…` label. A tab showing source is the point of this
   route, and `text/plain` cannot execute whatever the extension claimed.
3. Everything else downloads.

Tier 2 asks the bytes, not the extension: a prefix is checked for a NUL and for
UTF-8 validity. An extension table would need an entry per language to keep
`.svelte` or `.rb` from downloading and would still be wrong about a `.log` full
of binary — the same reason `/files/content` decides text-vs-binary by decoding.

`.svg` is deliberately never served as `image/svg+xml`: at that type a script
inside it runs in our origin, whereas the same file inside an `<img>` cannot. It is
text, so it lands in tier 2 and a tab shows its *source*. That is duller than
rendering the drawing and is the intended trade — the pane renders SVG through the
`/files/content` base64 path, inside an `<img>`, so nothing needs this route to
render it.

The filename in `Content-Disposition` is taken from the *canonical server-side
path*, never from the request, and is sent in both RFC 6266 spellings: the ASCII
`filename=` fallback plus the percent-encoded `filename*=UTF-8''`. Both, because a
CJK filename cannot be expressed in the plain parameter at all — a bare
`filename="报告.pdf"` is an invalid header value, which would have dropped the
disposition entirely.

Errors: `FILE_NOT_FOUND` (404), `POLICY_VIOLATION` (403) — including a symlink
resolving outside the workdir — and `INVALID_REQUEST` (400) for a directory.

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

**`overwrite` does not follow a symlink out of the workdir.** Containment for a
write is established on the *parent directory*, because the file usually does not
exist yet and canonicalization needs an existing path — which leaves the last
component unresolved. A pre-existing symlink there is inside the root by name and
points wherever it likes, and a plain write follows it, so `overwrite` was a write
outside the workdir arriving through the one component the check could not see.
An existing entry is now inspected with `symlink_metadata` and, when it is a link,
resolved and re-checked. A link resolving *inside* the workdir still writes through
to its target, because that is a write the agent itself can plainly make and
refusing every link would be stricter than the stated line; a dangling link is
refused, since where the write would land cannot be established and creating its
target would be inventing one.

`DELETE` was already safe from this by construction — unlinking a link never
touches its target — and its implementation asserts that a final escaping symlink
is "only ever unlinked, never opened or written to". That invariant held for
`DELETE` and not for this endpoint; it now holds for both. `POST .../directory`
refuses an existing target outright, so it was never reachable.

Errors: `POLICY_VIOLATION` (403) outside the workdir, including through a symlink
at the destination, `INVALID_REQUEST` (400) for an existing file without
`overwrite`, `ATTACHMENT_INVALID` (400) for malformed base64 or an oversized
payload.

#### `POST /api/v1/sessions/{id}/files/directory`

Create one directory.

```json
{ "path": "docs", "name": "drafts" }
```

`path` is the *parent*, relative to the workdir; empty means the root.

**Response 201:**
```json
{ "path": "docs/drafts", "absolutePath": "/Users/you/p/docs/drafts" }
```

Its own route rather than a mode of `POST .../files`: one takes a body of bytes and
the other a name, and a handler switching on which field is present is how an
upload with a typo'd key silently becomes a mkdir.

Non-recursive, and it refuses an existing path rather than succeeding quietly, so
"New folder" twice reports the collision instead of looking like a no-op.

`name` is **rejected, not rewritten**, when it is unusable — a separator, `.`,
`..`, a control character, over 255 bytes. Deliberately not the upload sanitizer,
which strips non-ASCII: that is right for a filename derived from an upload, where
the name is incidental, and wrong for a folder the user is naming, where `文档`
silently becoming `attachment` is data loss with no visible cause.

Errors: `INVALID_REQUEST` (400) for a bad or taken name, `POLICY_VIOLATION` (403)
outside the workdir.

#### `DELETE /api/v1/sessions/{id}/files?path=&recursive=`

Delete one file or one directory. **Response 204**, no body.

This is the only irreversible operation in the API. There is no trash to restore
from, because reaching the platform's trash means either a new dependency or
spawning `osascript`, and a browser page that can invoke a host process is a larger
concession than the delete itself. The guards are therefore the safety, and they
hold for any client, not only this project's UI:

- **The root is refused** with `403`. `path=""`, `"."`, and `"/"` all normalize to
  the workdir, and a client bug that sent one would otherwise erase the project.
  No legitimate request means "delete the working directory".
- **A non-empty directory needs `recursive=true`**, else `400`. Deleting one stray
  file and deleting a subtree are different intentions. An empty directory needs no
  flag, because there is nothing to lose.
- **A symlink is unlinked, never followed.** This one needs care: the read path
  canonicalizes, which resolves a link to its target, so deleting `link` would
  delete the *directory it points at*. So this endpoint canonicalizes the
  **parent** and appends the final component verbatim. Containment still holds —
  every `..` and every symlinked directory in the path is resolved and checked —
  and a final component that is itself an escaping symlink is only ever unlinked.

The UI sends `recursive` for any directory and states in one dialog that the
contents go too. Confirm-fail-confirm would be two dialogs for one decision, and
Finder does not ask twice either; the backend guard stays as protection against a
client bug rather than as a step in the flow.

Errors: `FILE_NOT_FOUND` (404), `FORBIDDEN` (403) for the root or an escape,
`INVALID_REQUEST` (400) for a non-empty directory without `recursive`.

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
listed, because `~/.sylph` is a legitimate workspace the product points at, and
because a directory a user chose to name should not be hidden from them.

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

Process-local HttpOnly cookie. Required for every call except `GET /api/v1/health`
and `GET /api/v1/auth/token`, which are exempt inside the middleware rather than
registered outside it, so the exempt list sits next to the checks it bypasses.

- Cookie: `sylph_web_auth={random_token}`; `Path=/`, `HttpOnly`, `SameSite=Strict`
- Origin must match loopback (`127.0.0.1`, `localhost`, `[::1]`)
- Host header validated against the same allowlist

#### `GET /api/v1/auth/token`

Bootstraps the cookie for a loopback client and answers `{"ok": true}`. No
credential is exchanged: the backend hands the cookie to any request that already
passes the loopback `Host`/`Origin` checks, which is why a browser on the same
machine is never prompted. Reaching the backend at all is the authorization.

**`SameSite=Strict` is why the frontend must proxy `/api/v1/*` same-origin.** A
browser will not send the cookie to a different port, so pointing the UI straight at
`:4317` fails every authenticated call while `curl` succeeds.

**The token is regenerated on every backend start**, which invalidates open browser
sessions. That is the design — the cookie is process-local, and a token that
outlived the process would be a persisted credential with no way to revoke it. Do
not "fix" it by writing the token to disk; reload the page instead.

Mutations additionally require a loopback `Origin`, which is what makes a tunnelled
or LAN origin read-only unless it comes through a proxy that rewrites `Origin` to
loopback. See `docs/development.md`.

## OpenAPI Specification

Full machine-readable spec: `docs/api-v1.openapi.json` — 22 paths, covering every
route the router registers.

Generate TypeScript types:
```sh
npx openapi-typescript docs/api-v1.openapi.json -o frontend/src/types/api.ts
```

The frontend does not currently consume generated types; `frontend/lib/api.ts`
declares its own. Keep the spec authoritative for the wire contract regardless — a
route added to `backend/src/lib.rs` without a matching path here is the gap that let
`DELETE /sessions/{id}` and `/fs/*` go undocumented.
