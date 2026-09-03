# API Design

Sylph Web exposes a versioned REST API with SSE streaming for real-time updates.

This is a reference organized by resource, searched rather than read end to end.
`docs/api-v1.openapi.json` is the machine-readable form and is authoritative for schemas.

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

## A session id in a path is the identity triple

A session is `(environment, agent, native_id)` (`docs/architecture.md` § Identity).
On the wire and in a path segment it is encoded `<environment>:<agent>:<native_id>`
and **percent-encoded** as one segment, so `{id}` below is that string throughout.

Two rules a client must not break:

- **Do not split on `/`.** Native ids and vendor values contain slashes — 350 of
  `pi`'s 372 model ids do — which is why the delimiter is `:` and why this repo already
  paid for the other choice once.
- **Treat it as opaque.** A client echoes it back; it never parses it to learn which
  agent or environment a session belongs to. Those are separate fields on the session
  object, and reading them from the id is how a rename or an encoding change breaks a
  client silently.

There is no server-minted UUID. `POST /sessions` therefore starts an agent and returns a
process-local draft handle rather than reserving a row. Capabilities and configuration can
be read or changed through the draft routes, and the first turn promotes it to a normal
session. A draft handle is never a session id and is not included in listings or
native-history browsing.

## Core Endpoints

### Health & Diagnostics

#### `GET /api/v1/health`

Health check with version information.

**Response 200:**
```json
{
  "ok": true,
  "apiVersion": "1",
  "schemaVersion": 7,
  "environments": [
    {"id": "env_7fk2", "name": "laptop", "connected": true,
     "agents": [{"name": "claude", "version": "2.1.247"}, {"name": "pi", "version": "0.84.3"}]},
    {"id": "env_k9x1", "name": "build-box", "connected": false,
     "disconnectedSinceMs": 1724057000000}
  ]
}
```

#### `GET /api/v1/diagnostics`

Detailed system state (sessions, turns, agents, retention).

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
    "withheldReasons": {"environment_unreachable": 48, "source_absent": 4},
    "terminal": 41,
    "failed": 0,
    "blocksPruned": 318,
    "blobsSwept": 4,
    "environmentsReachable": {"env_7fk2": true, "env_k9x1": false},
    "bytesHeld": 4183740928,
    "sessionsMeasured": 357,
    "bytesGrewBy": 12043776,
    "largestSessions": [
      {
        "sessionId": "env_7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
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
agent failure messages.

`retention` is the last completed pass, or `null` before the first one. Two things
about its shape are load-bearing, both covered in `docs/storage.md`:

- **`withheld` beside `terminal`** is what separates correct behaviour from a broken
  pass, and `withheldReasons` is what makes the difference legible. A terminal session is
  *supposed* to keep everything; a backed session that keeps withholding means no
  environment is confirming it. Counting withheld from the authority's own reported reason
  rather than from an absent bound is what keeps "answered, nothing safe yet" distinct
  from "nobody was asked".
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
  ]
}
```

Four decisions:

- **`source` and `envVar` on every entry.** A resolved value alone cannot
  distinguish "this path is wrong" from "the path I set is not being read", and
  those need opposite fixes. Naming the variable in the row means acting on it does
  not require consulting a table elsewhere.
- **`exists` is absent, not `false`, for anything that is not a path.** A log level
  rendered as missing reports a working setting as broken.
- **No `bundledAgentCandidates`, and no `paths.agents_dir` entry.** Both are deleted
  with the adapter directories: adapters are compiled in, so there is nothing to search
  and no path that could be mistyped. What replaced the question they answered — "why can
  this install run no agents" — is per-environment availability on
  `GET /api/v1/environments`, which names the missing CLI rather than the directory it was
  not found in.
- **Read-only, with no `PUT`.** These values name filesystem paths, and an
  environment's `browse_roots` bounds where an agent may be spawned. A loopback HTTP
  surface that could rewrite that is a materially larger threat-model question than one
  that displays it, and the file is the durable, reviewable record. The panel shows the
  values and the path to open.
- **Authenticated.** Every value is an absolute local filesystem path. `/health`
  stays unauthenticated for supervisor probes and reports booleans only.

Built per request rather than cached: every `exists` field is a filesystem fact, and
a cached report would keep telling a user a directory is missing after they created
it. It is a handful of `stat` calls against the page cache.

There is no adapter-search report here any more, because there is no search: adapters
are linked into the envagent and an agent's availability is whether its CLI is on that
machine's `PATH`. `GET /api/v1/environments` reports that per machine, and
`sylph-envagent doctor` reports it locally. What the old report existed for — making
"no agents" actionable rather than an empty list with no cause — is answered by naming
the CLI that was not found.

See `docs/configuration.md` for the settings themselves.

### Agents and environments

#### `GET /api/v1/environments`

Every paired environment, whether it is connected, and what it can run.

**Response 200:**
```json
{
  "environments": [
    {
      "id": "env_7fk2",
      "name": "laptop",
      "connected": true,
      "origin": "self-installed",
      "reachableVia": null,
      "agents": [
        {
          "name": "claude",
          "version": "2.1.247",
          "available": true,
          "flags": {
            "model": {"choices": ["opus-5", "sonnet-5"], "description": "Model to use"},
            "effort": {"choices": ["low", "medium", "high", "xhigh", "max"]}
          },
          "supports": ["interrupt", "steer", "approval", "subagent", "slash",
                       "set_config", "context", "fork", "watch"]
        }
      ]
    },
    {
      "id": "env_k9x1",
      "name": "prod-builder",
      "connected": true,
      "origin": "ssh-bootstrapped",
      "reachableVia": "env_7fk2",
      "agents": [{"name": "codex", "version": "0.9.1", "flags": {}, "supports": [], "available": true}]
    }
  ]
}
```

**Which agents exist is per environment, not global.** A user picking an environment
with no `codex` on it should learn that before creating the session, not after. There is
no top-level agent list, because the answer differs per machine and a merged list would
be true of nowhere.

**`reachableVia` is not decoration.** A non-null value means this environment is
reachable only while the named one relays for it, so **control dies when that machine
leaves** (`docs/environments.md` § SSH-bootstrapped). A UI rendering it identically to a
directly-connected environment would promise AFK continuity it cannot keep.

**Availability is `PATH`, not a handshake.** An agent is available when its CLI is on
that machine's `PATH` and reports a version. There is no discovery pass, no adapter
directory, and therefore no `broken` map — the two total failures that mechanism produced
are recorded in `docs/session-protocol.md` § What changed. "Why can I not use this
agent" now has one answer with one fix.

**`flags` is the spawn-time option schema** — what can be chosen before a session
exists. Anything adjustable on a *live* session is a config facet instead, reported by
`GET /sessions/{id}/capabilities`, and a session's real slash-command list arrives there
too because it depends on files the CLI reads at startup. Nothing in the UI hardcodes a
model list, which is why `codex` correctly shows effort with no model selector.

**`supports` is a closed vocabulary that drives the UI directly** — a composer must not
offer interrupt to an agent that cannot interrupt.

#### `POST /api/v1/environments/pairings` — designed, not built

Mint a short-lived, single-use pairing code (`docs/environments.md` § Pairing). The
envagent exchanges it for a durable credential and is assigned its opaque id.

**Response 201:** `{"code": "…", "expiresAtMs": 1724054460000}`

#### `DELETE /api/v1/environments/{id}` — designed, not built

Unpair. Sessions recorded from that environment stay readable — their history is in the
server's store — but their **control** facet reports unavailable, and retention treats
them as terminal, because no reachable native source can confirm a bound
(`docs/storage.md`).

Both arrive with `crates/envagent`; there is no route for either today, and the local
environment needs no pairing.

### Sessions

#### `GET /api/v1/sessions`

One list, one store. There is no live/archived split and no deduplication step,
because native history is parsed into the same events a live turn emits and lands in
the same store (`docs/architecture.md` § One authority).

**Query parameters:** `limit` (default 50, max 200), `agent`, `environment`,
`workdir`.

**Response 200:**
```json
{
  "sessions": [
    {
      "id": "env_7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
      "environment": "env_7fk2",
      "agent": "claude",
      "nativeId": "9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
      "workdir": "/Users/hsw/Workspace/project",
      "title": "Feature implementation",
      "injectIdentity": true,
      "fidelity": "stream",
      "flagsJson": "{\"model\":\"opus-5\",\"effort\":\"high\"}",
      "status": "idle",
      "createdAtMs": 1724054400000,
      "updatedAtMs": 1724058000000
    }
  ]
}
```

`id` is the encoded triple and the route key. `environment`, `agent`, and `nativeId`
are also given as separate fields **so a client never parses the id** — see § A session
id in a path.

The environment display name is resolved from `GET /api/v1/environments` rather than
duplicated on every session row. It is mutable metadata, never a key: a rename must not
destroy identity (`docs/environments.md` § Pairing).

**`fidelity` is what a renderer must read before drawing a duration.** `stream` means
every delta and its arrival time is recorded; `blocks` and `messages` mean the source
recorded less, and no care recovers a timing that was never written. A waterfall over
message-fidelity data presents equal-length bars as measurements.

**`flagsJson` is a JSON *string*, not a `flags` object.** Requests take `flags` as an
object; responses report the stored column and the client parses it. This document
showed `flags` here for a release, so a client written from it read `undefined`.

**A path filter is an exact match after normalizing trailing slashes, never a
substring.** The store this replaced compiled `workdir` to `LIKE '%' || ? || '%'`, so
asking for `/w/app` also returned `/w/app-old` and every subdirectory.

#### `POST /api/v1/sessions`

**This starts an agent.** It does not reserve a durable row. A session has no durable
identity until an agent has assigned one (`docs/architecture.md` § Identity), so the
pre-message state is an in-memory draft rather than a session record.

**Request:**
```json
{
  "environment": "env_7fk2",
  "agent": "claude",
  "workdir": "/Users/hsw/Workspace/project",
  "title": "Feature implementation",
  "injectIdentity": true,
  "flags": {"model": "opus-5", "effort": "high"}
}
```

`injectIdentity` decides whether `SOUL.md` is appended, defaults to `true`, and is
**refused by `PATCH`**: a transcript records what produced it, so rewriting the flag
would rewrite what the stored history means. It is a default, not a policy — see
`GET /api/v1/soul` for the text it governs.

**Response 201:** a process-local `draftId` plus the launch metadata.  This is not a
session id and is not listed anywhere.  The draft may still read or change its
working-directory files through the `/sessions/draft/{id}/files/*` routes, because the
prepared link already owns the contained `workdir`; those routes disappear with the
draft and never create history.  The first non-empty turn sent to
`/sessions/draft/{id}/turns` atomically promotes the link and returns the durable session.

**What changed, and what it deletes.** The previous shape wrote a row with a
server-minted UUID and left the agent unstarted, which produced a class of session with
no work in it. Three things go away with that: `DELETE /api/v1/sessions/{id}` for
empties, the frontend probe that called it on unmount, and the window in which a
session existed with no native id and therefore could not be resumed. A failed spawn now
reports at the start screen instead of leaving a dead row behind.

**`workdir` is bounded, and the bound now lives in the envagent.** It must resolve
inside that environment's `browse_roots` or its Quick Start directory, checked on the
canonical path. Only the machine holding the path can canonicalize it, know whether a
component is a symlink, or resolve `..` — a server checking a remote path would be
matching a string against a filesystem it cannot see (`docs/configuration.md`).

The reason it is bounded at all: this field is the `current_dir` of a spawned agent, and
a coding agent reads and writes the tree it is pointed at, so it decides **where
arbitrary code runs**. Existence — the only property it was once checked for — says
nothing about whether it should.

The **canonical** path is recorded, not the string as sent, so two spellings of one
directory do not become two workspaces. Note the consequence, which bit once: anything
that groups, compares, or displays a path must use the same spelling. Comparing a
canonical `workdir` against a *configured* Quick Start path reported every Quick Start
session as not-Quick-Start, and nothing errored — the row and the sessions it counts
simply stopped being the same directory. On macOS this is not an edge case: `/tmp`,
`os.tmpdir()`, and everything under `/var` all differ from their canonical form, so the
bug is invisible on Linux CI and certain on a developer's machine.

**Errors:**
- `400` — unknown agent, workdir missing or not a directory, flags validation
- `403` — `POLICY_VIOLATION`: the workdir exists but is outside that environment's
  permitted roots. Distinct from `400` on purpose — the request is well-formed and the
  fix is in configuration, not in the request. The message lists what is permitted.
- `502` — `AGENT_START_FAILED`: the CLI could not be started or did not answer the
  initial capability handshake. A draft may start without a native id; identity is
  acquired and checked only when its first real turn is promoted.
- `503` — `ENVIRONMENT_UNREACHABLE`: no envagent is connected for that environment

A session already recorded outside the permitted roots stays fully readable. Nothing
re-checks a recorded workdir on read, list, or resume — a rule that applied on read
would make every session created under wider configuration unreadable, which is a worse
failure than the hole it closes.

#### Prepared-link routes

The `draftId` returned by `POST /sessions` is an opaque, process-local handle. It is
valid only while the prepared link is alive and must never be passed to a durable
session endpoint. These routes expose the capabilities needed to configure a link
before the first message, without inventing a second session identity:

- `GET /api/v1/sessions/draft/{id}` returns the launch metadata (`draftId`, environment,
  agent, workdir, title, and `injectIdentity`).
- `GET /api/v1/sessions/draft/{id}/capabilities` returns the same typed `LinkState`
  surface used by durable sessions. The adapter owns all vendor-specific model,
  thinking, permission, and command details.
- `POST /api/v1/sessions/draft/{id}/config` accepts `{ "id": "...", "value": ... }`
  and returns the updated `LinkState`. The id and value are opaque to the server; the
  adapter validates and translates them.
- `POST /api/v1/sessions/draft/{id}/turns` accepts the normal turn payload. A non-empty
  text, declared command, or attachment is the promotion boundary: the adapter first
  obtains the vendor's native id, then the server atomically writes the durable session
  and first turn and returns `SessionEnvelope`. An entirely empty payload is rejected
  and leaves the draft disposable.
- `DELETE /api/v1/sessions/draft/{id}` closes the link and removes draft-only temporary
  resources. It always leaves no journal row; calling it for an already-discarded draft
  is harmless.

Draft file routes under `/sessions/draft/{id}/files/*` use the same containment rules as
durable file routes and disappear with the draft. Reading or changing files never
promotes a draft and never creates history.

#### `POST /api/v1/sessions/adopt`

Bring in a session an agent already has on some environment, from that agent's own
native store. Replaces `POST /sessions/import`.

**Request:**
```json
{
  "environment": "env_7fk2",
  "agent": "claude",
  "nativeId": "9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
  "watch": true
}
```

`watch: true` follows the store as it grows, so a session running in a terminal right
now appears and keeps updating; `false` parses one pass of what is already recorded
(`docs/environments.md` § Watching and adopting). `title` may be supplied to override
the derived one.

**Response 201:** the session object, with `fidelity` reflecting what the vendor's store
actually holds — `blocks` or `messages`, never `stream`.

**Policy-relevant fields are derived from the store, not from the request.** The
working directory comes from the session's own recorded environment context; the title
falls back to the opening user turn. `injectIdentity` defaults to **false** here,
because the transcript so far was produced without it — but it is settable, since
adopting a terminal session and injecting from that point on is a coherent thing to
want.

**The recorded working directory is bounded exactly like a created one**, and this is
not belt-and-braces. A machine's store holds every session any agent ever ran there, so
an unbounded adopt is a *general* way to start an agent anywhere: find a session that
ran in the target directory, adopt it, send a turn. That the path came from the store
rather than from the request changes who chose it, not what happens next.

**The title fallback must skip injected envelopes.** The first `user` event is usually
not what the user typed — agents inject their own context as leading pseudo user turns
(`<environment_context>`, `<skills_instructions>`, memory files, `claude`'s `Caveat:`
preamble). Taking it verbatim labels most sessions with boilerplate byte-identical
across every session of that agent, so skipping those envelopes is what makes a label
recognizable at all. Keep the rule narrow: treating a leading `You are ` as boilerplate
discards genuine labels, because subagent briefs legitimately open that way.

**Errors:**
- `400` — no such agent on that environment, or the recorded workdir no longer exists
- `403` — `POLICY_VIOLATION`: the recorded workdir is outside that environment's roots
- `404` — `NATIVE_SESSION_NOT_FOUND`: that environment's store has no such session
- `409` — `SESSION_ALREADY_PRESENT`: adopting twice would produce two rows for one
  conversation
- `422` — `NATIVE_SESSION_NOT_RESUMABLE`: the store recorded no working directory
- `503` — `ENVIRONMENT_UNREACHABLE`

Those are distinct because they have distinct fixes: nothing to do, widen
configuration, wrong id, already done, unusable source, and bring the machine online.
Collapsing any pair sends a reader to the wrong layer.

#### `GET /api/v1/sessions/enumerate`

What native sessions an environment holds, without adopting any of them. This is what
makes adopt offerable in a UI rather than requiring a user to know a vendor id.

**Query parameters:** `environment` (required), `agent`, `workdir`, `limit`, `cursor`.

Answered by the envagent from its local stores. A session already adopted is marked as
such, so a picker can show it rather than offering a duplicate that would `409`.

#### `GET /api/v1/sessions/{id}`

Get one session.

**Response 200:**
```json
{ "session": { /* the same shape a listing row has, below */ } }
```

**That is the whole response.** `api::sessions::get` returns `SessionResponse { session }`
and nothing else — no turn summary and no turn counts, on this or any other route. A client
that needs the turn list asks `GET /sessions/{id}/turns`.

The response exposes the vendor id as `nativeId`, alongside the encoded `id` and the
separate `environment` and `agent` fields. There is no second server-minted id to
translate, and session timestamps are `createdAtMs` / `updatedAtMs`.

#### `PATCH /api/v1/sessions/{id}`

Change agent flags or the title of an existing session.

```json
{ "flags": { "model": "opus", "effort": "high" }, "title": "Auth refactor" }
```

Both fields are optional; an omitted field is left untouched. `flags` is a
**complete replacement set**, not a merge — the caller has already validated the
whole set, and merging would let a stale key survive a change the user believed
was total. An empty object clears them.

Note the asymmetry: `flags` is what a *request* carries, while a session
*response* reports `flagsJson`, a JSON string the client parses
(`models/session.rs`, `frontend/lib/api.ts:42`). Reading a session and echoing its
`flags` back is therefore not a round trip.

**Why flags are validated here rather than at turn time.** `POST .../turns` filters
the stored set against the agent's declared flag schema and silently drops what it
does not recognize. That is right there: a descriptor can change under a session
created before it, and the turn should still run. It is wrong for a user action —
a model chosen in the composer that the agent never declared would be accepted,
stored, dropped at turn time, and the turn would run on the default with nothing
anywhere saying why. Validating on write turns that silent no-op into an error the
UI can show against the control that caused it.

**Identity is not patchable.** `environment`, `agent`, and `nativeId` *are* the
session's identity, so changing one does not modify a session, it names a different one.
`injectIdentity` is refused for a different reason: a transcript records what produced it,
so rewriting the flag would rewrite what the stored history means. There are no `origin`,
`identityMode`, or `lifecycleMode` fields — `docs/architecture.md` § One class of session
is authoritative for
them.

Errors: `SESSION_NOT_FOUND` (404), `INVALID_REQUEST` (400) for an undeclared flag
key, a value outside a declared enumeration, or an agent that is no longer
installed.

#### There is no `DELETE /api/v1/sessions/{id}`

**Nothing deletes a durable session.** A prepared draft is disposable through its draft
route because it has no journal row; no route deletes a session *with* history. Delete,
move, and export of durable records are absent; see the roadmap.

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
    "sessionId": "env_7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
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
    "sessionId": "env_7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
    "status": "completed",
    "origin": "user",
    "requestText": "Implement feature X",
    "commandInvocationId": null,
    "idempotencyKey": "…",
    "errorCode": null,
    "errorMessage": null,
    "createdAtMs": 1724059200000,
    "startedAtMs": 1724059205000,
    "completedAtMs": 1724059312000,
    "stoppedAtMs": null
  }
}
```

**`origin` is `user` or `agent`, and a client that ignores it renders an answer to a
message the user never wrote.** This is turn-level provenance, not a session class. An agent may start a turn of its own to report work that
outlived the turn which dispatched it, and such a turn's `requestText` is empty — which
is otherwise indistinguishable from a user sending nothing. `docs/session-protocol.md`
§ An agent may start a turn.

**There is no per-turn exit code**, because an adapter holds a session for its lifetime and
a turn is a message on its link. A turn's outcome is `status` plus
`errorCode`/`errorMessage`; nothing reads a process exit.

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

Every event is ordered by `seq`. New records also expose `arrivalAtMs` (the server
receipt clock) and optional `recordedAtMs` (the vendor clock); `createdAtMs` remains
for compatibility. A duration may subtract only endpoints in the same clock domain.

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

**What happens, in order.** A turn runs inside a session-long link rather than its own
process, so **nothing signals or kills a process to stop a turn** — that would end the
whole session to stop one message:

1. **The turn is settled first**, as `Interrupted`, before the link is touched at all.
   This ordering is the point rather than an optimization: every way of ending a turn
   asks the agent to stop producing, and an agent that stops producing is one its
   drain task observes closing — which reports `LINK_LOST`. Claiming the outcome first
   means that attempt finds the turn already terminal and does nothing, so a
   deliberate stop is never reported as a transport failure. Settling *after*
   interrupting is a race rather than a fix.
2. **The adapter is asked to interrupt.** Settling first is also what makes stop
   unconditional — the record does not wait on the agent to answer, so a user asking
   to stop is never told the agent declined.
3. **If interrupt fails or is unsupported, the link is torn down.** That costs the
   session's live context, which is why it stays the fallback rather than the path.
   Link teardown itself is `close` on the wire, a 500 ms grace period, then `SIGTERM`
   to the process group; escalation is unconditional because signalling an
   already-exited group is defined to be harmless. There is no `SIGKILL` step.

### Links

A **link** is a live channel to a session's agent, held by the envagent for the
environment that session runs in. It is not the session: a session with no link is
complete, resumable, and searchable, and links are torn down on idle (30 minutes) and do
not survive a server restart or an envagent disconnect. Everything in this subsection is
therefore about *live* state, and a client must render it as live state rather than
promising it unconditionally.

These five routes had no section in this document at all until now, which is why they
are grouped rather than filed under Sessions: `capabilities`, `link`, `interrupt`,
`steer`, `config`, plus the session event stream below.

#### `GET /api/v1/sessions/{id}/capabilities`

Returns `LinkState`. **Establishes a link as a side effect** — this is the read that
most clients use to attach, because a UI asking what a session can do is a UI about to
offer it.

A failure here is **200, not 5xx**, carrying an unavailable `control` facet with a
typed reason.
That is the endpoint's one non-obvious decision and it is deliberate: a missing adapter
or a refusing agent is the *agent's* failure, not the request's, and the session remains
valid with readable history — a 500 would make the whole session look broken. The two
facets make the reason explicit instead of flattening it into an unexplained boolean.

Two consequences a client must handle:

- **Do not cache this response indefinitely.** A detached-with-reason 200 under
  `staleTime: Infinity` makes one failed handshake permanent for the life of the page.
  Cache by reason: `no_agent` is cacheable like a success, because no retry installs a
  vendor CLI; a failed handshake must be allowed to go stale.
- **An empty capability surface means "Sylph cannot reach it", never "there is nothing
  there".** These coincide for a session Sylph launched and are opposite for one running
  in a terminal, where the process is alive and simply not ours to drive.

**`LinkState` carries two typed facets, `observe` and `control`, each able to report
unavailable with a reason.** This is the shape rather than a third link state, because
the vendors do not sort into three buckets: a terminal `pi` is observable and
undrivable, `codex` is both, `claude` is observable and semi-drivable behind a human
approval gate (`docs/agents.md`). Two failure codes worth naming, because neither
existed and both are the common case now:

| Code | Means |
| --- | --- |
| `environment_unreachable` | no envagent connected for this session's environment — observe still answers from the store |
| `not_ours_to_drive` | the process is alive under someone else, and this vendor offers a second party no control channel |

**These differ in whether a retry can change the answer**, and a client must not cache
them alike. `not_ours_to_drive` and `no_agent` are cacheable like a success — no retry
teaches a vendor a control channel it does not have, or installs a CLI. But
`environment_unreachable` **must be allowed to go stale**, because a laptop wakes and an
envagent redials; cache it like a success and a reachable machine goes on being reported
offline for the life of the page.

`crates/protocol`'s `UnavailableReason::is_retryable` is the single definition, and a
client mirrors it rather than re-deriving the rule inside a cache policy. The vocabulary
itself is authoritative in `crates/protocol/src/link.rs`.

#### `POST /api/v1/sessions/{id}/link`

Establishes a link eagerly and returns `LinkState`. Worth its own operation because
establishment is slow — an agent CLI loads a project — so a UI that knows a user is
about to type can pay that cost before they finish rather than after they send.

Unlike `capabilities`, a failure here **is** an error (`LINK_FAILED`), because
establishing was the entire request rather than a side effect of one.
`ENVIRONMENT_UNREACHABLE` (503) is separate from `LINK_FAILED` (502): one is fixed by
bringing a machine online, the other by looking at the agent.

#### `DELETE /api/v1/sessions/{id}/link`

Tears the link down without ending the session, and returns the resulting `LinkState`.
History is unaffected. The next turn re-establishes.

#### `POST /api/v1/sessions/{id}/interrupt`

Asks the agent to stop producing. Session-scoped rather than turn-scoped, and so are
`steer` and `config`: all three act on the *agent*, and the session is what holds a
link. A turn-scoped route would force a caller to know a turn id in order to interrupt
the only turn running.

#### `POST /api/v1/sessions/{id}/steer`

```json
{ "text": "actually, use the existing helper", "turnId": "t_abc" }
```

`turnId` is optional, because a client with no turn in view is steering "whatever is
running", which is a coherent ask rather than a missing field.

#### `POST /api/v1/sessions/{id}/queue`

```json
{ "text": "then run the tests" }
```

Answers `{"queued": true}`. Requires `Capability::Queue`; an agent without it answers
`UNSUPPORTED_FIELD`, which a client renders by retiring the control rather than leaving
it clickable.

**Not `steer` with a flag**, and the difference is *when the agent reads the text*
rather than how it is delivered. A steer is a correction, so it carries a turn
precondition: text that misses its turn lands in the wrong conversation, and naming the
turn is what turns that into a refusal. A queued follow-up is *for* after the turn, so
it has no such precondition — a turn boundary crossing underneath it changes nothing,
and queueing with nothing running is a coherent ask that becomes the next turn's opening
text. Hence no `turnId` here at all, where one endpoint serving both meanings would need
a field that is required for one and meaningless for the other.

That shape is what let a vendor claim the wrong word for what it does: `claude` declared
`Steer` while its only mid-turn channel is stdin, which `--input-format stream-json`
reads through one iterator the turn loop awaits — so the text is not read until the turn
ends. A user's correction arrived after the thing it was meant to prevent, with every
layer reporting success. All three agents have `Queue` and each spells it differently;
only `codex` and `pi` also have a real steer (`docs/agents.md` § Capability matrix).

#### `POST /api/v1/sessions/{id}/config`

```json
{ "configId": "model", "value": "anthropic claude-opus-5" }
```

A typed set, so `/model` and `/permissions` are not prose sent as a prompt. Four rules
this endpoint's history earned, each of which cost a debugging round:

- **`value` is opaque.** It is a token the agent enumerated and the client echoes back,
  never something to parse. `pi`'s model ids contain `/` in 350 of 372 cases, so the
  composite encoding uses a space — and only because the value is opaque can that
  encoding change without a client change.
- **A refusal distinguishes two codes.** `UNSUPPORTED_FIELD` means no such option and
  justifies disabling the control; `INVALID_VALUE` means *this value, right now* and
  must leave it usable. Flattening both into one retires a picker whose other values
  work.
- **A client must render the refusal.** A mutation with only an `onSuccess` path leaves
  a rejected set invisible: the cached surface is untouched, the control re-renders from
  it, and the sole observable outcome is that clicking did nothing. Every layer behaves
  correctly and the symptom points at the one that is not at fault.
- **The reported `currentValue` comes from the vendor's confirmation, never from the
  request.** A vendor's `success` is not a promise it did what you asked: measured on
  `claude` 2.1.246, `manual` answers success while confirming `default`.

An accepted setting is remembered for a durable session only when its adapter declares a
safe launch-flag equivalent. That mapping belongs to the adapter: the server does not
know that Claude's live `permissionMode` is spelled `mode` at launch, nor should it persist
runtime-only controls such as output style. The stored `flagsJson` therefore represents
the next-link launch preferences, while `currentValue` remains the vendor-confirmed live
state. A prepared draft keeps the same adapter-owned subset in memory until promotion.

#### `GET /api/v1/sessions/{id}/events`

Session-scoped facts as SSE: a link attaching or dying, capabilities changing, a request
raised or retired. Distinct from the turn stream, which exists only while a turn does —
these happen with no turn running, which is precisely when a client would otherwise have
no way to learn them.

Two properties worth relying on. A **pending request is published on every transition**,
including its departure, from one method rather than three call sites — a set that only
ever grew is what made an answered question sit on a second client until reload. And the
payload is the **row**, carrying `status`, not the transition, so a client reconciles
against a state rather than replaying a vocabulary of verbs, and a status added later
needs no client change.

#### `GET /api/v1/sessions/{id}/trace/events`

The reconnectable, session-scoped trace. This is the stream a transcript view should
hold while a session is open: it carries normalized events from every turn, not only
link/request invalidations. Stable rows are replayed from `journal.db` or
`history.db`; the live channel contributes the bounded ephemeral tail and then stays
open across turn boundaries.

Each event includes `arrivalTurnId`, the turn on which the server received it. That is
not trajectory ownership: a child can finish after its parent turn, so the frontend's
trace index uses the first trajectory declaration (or promoted host) as `ownerTurnId`
and keeps `arrivalTurnId` only as provenance. This is what prevents a late Codex child
close from becoming a synthetic `origin: agent` turn or a second outer card.

The older `/sessions/{id}/events` endpoint remains a cheap state-invalidation stream
for capabilities, links, and pending requests; it intentionally has no trace replay.

#### `GET /api/v1/sessions/{id}/replay`

Server-paced reconstruction of a session's history. It is session-scoped and lives on
the server for one reason: reconstruction is subtle enough that doing it per frontend
would let two frontends disagree about what the same history means.

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
      "environment": "env_7fk2"
    }
  ]
}
```

Ordered newest-first by `lastUsedMs`. The Quick Start directory is always
present, even with no sessions, because the sidebar section and the settings row
exist before the first quick session is created.

**A workspace is scoped to an environment.** Two machines both having
`/home/user/app` is not a collision, because a path is only ever read alongside the
environment that named it (`docs/architecture.md` § Sessions do not migrate). Grouping
by path alone would merge two unrelated directories into one row.

**Comparing a recorded path against a configured one needs the same spelling.**
`workdir` is stored canonicalized, so comparing it against a configured Quick Start path
reported every Quick Start session as `isQuickStart: false` — nothing errored, the row
and the sessions it counts simply stopped being the same directory. On macOS this is
certain rather than an edge case, and invisible on Linux CI.

### Lineage, folding, and labels

**There is no `/archive/*` namespace.** Reading history is `GET /sessions` and
`GET /sessions/{id}`, because one store holds it (`docs/architecture.md` § One authority);
what would be a separate read path is query parameters on the listing above.

#### A listing folds delegated work into its parent

**Roughly half the sessions in a real store are delegated work**, so a listing that does
not fold them is half noise. That ratio has held across re-measurements while the absolute
counts moved, which is why the fold is a design rule rather than a tuning choice.

So a row carries `delegateCount` and `delegates`, and the fold has one exception that
is not optional:

**A cross-directory child is promoted to a row of its own.** 484 of 2238 delegates ran
somewhere other than their parent — typically a worktree or a subdirectory — and folding
those into a parent that is *not in the directory being listed* deletes them from the
only listing that would ever show them — measured, it emptied dozens of directories, one
of them holding hundreds of sessions. In its own directory a delegated session is not
delegated work, it **is** the work.

`delegates` carries the children themselves, and `delegateCount` is always its length.
A count alone is a dead end: half the store is delegated work, so a row reading "13
delegates" names sessions the client has no way to reach. The fold already visits every
child to count it, so identity costs nothing beyond the bytes. Each entry is
deliberately thinner than a listing row, and `cwd` appears only when the child ran
somewhere other than its parent — the case worth showing.

#### A fork and a delegation are different relationships

`parentSession` names one column, and the row it points at says which relationship it
is. They must not be presented the same way:

- **Delegation** folds into the parent as a count.
- **A fork** replays its origin's entire prefix and continues past it, so the two share
  every early user turn and read as one conversation resumed. Listing both shows it
  twice under one label, so **one survives** — and which one cannot be decided in advance:
  of two real local cases, one had the bulk of its later work in the fork and the other in
  the origin. Ordering by end time descending makes the survivor whichever is reached
  first.

`forkOf` names the suppressed origin on a surviving fork row. Within one response the
loser is already dropped, but **a pair can straddle two pages** — measured pairs sit
hundreds of rows apart — and the server keeps no state between requests. A client accumulating
pages must therefore drop a row whose `forkOf` names a session it has already shown, and
equally a row that a previously-shown fork names.

#### Lineage is derived from the listing, not asked per row

**Per-row lineage lookups are two orders of magnitude more expensive per fact than
reading the listing**, measured against a full local store: one session's metadata costs
milliseconds and a few kilobytes, so a single page spends a substantial fraction of a
second on lineage alone and a whole-store scan takes tens of seconds — while the entire
listing that already carries most of those facts returns in a fraction of one.

So **derive lineage from the listing and ask per row only for what the listing cannot
carry.** The fold caches per parent for the same reason. Finding one session's children
uses an id-prefix test over a single pass, which catches most delegates; the remainder
record only a parent id and are reached from their parent's directory listing rather than
by paying for the scan.

Re-derive these figures before relying on them (`docs/README.md` § How to write these
documents). The *ratio* is the durable part, and it is what the design rests on.

#### An id-suffix convention is a fallback, not a lineage signal

Some agents name a delegated session with a suffix derived from its parent, and that
catches most of them. It **misses those that record only a parent id** — `codex` among
them, measured from its own store: a rollout with `parent_thread_id` set,
`thread_source: subagent`, and no suffix at all. Those sat at the top level of every
listing until lineage started reading the store's own recorded parent. The suffix
survives only for when that lookup fails.

Attribution to the *specific* delegating call was measured and **rejected**: `claude`
names a child `<parent>/agent-a<name>-<hash>`, but the large majority of those suffixes
are hash-only, so matching recovers a small fraction of delegations and misattributes on a
collision. Structure that is right for one delegation in ten is worse than an honest flat
view, because nothing on screen distinguishes the two. Delegation is therefore
session-level in both directions.

#### A label falls back through three sources

1. **The vendor's own recorded title.** `claude` records one for ~73% of sessions;
   `codex` and `pi` record none — so the fallbacks are the common path, not the
   exception.
2. **The opening *typed* message**, skipping leading harness-injected envelopes
   (`<environment_context>`, `<skills_instructions>`, memory files, `Caveat:`). They are
   byte-identical across every session of an agent and identify nothing.
3. **An envelope's `summary` attribute.** Some delegated sessions have no typed turn at
   all — every one is a `<teammate-message … summary="…">` block — but that attribute is
   human-written and describes the task. XML entities in it are decoded.

Together those three label nearly every session in a real store; the title and typed ask
alone leave a visible fraction unlabelled, and rule 3 is what closes most of the rest.

#### A paged scan is bounded in rows, not pages

A directory filter also matches paths beneath it, and those sort by recency rather than
depth, so a parent directory's own first exact match can land a thousand or more rows in —
past which a page cap gives up and reports a directory with dozens of sessions as
**empty**. Bound such a scan in rows examined.

### Search

```
GET /api/v1/search?q=<text>&limit=20&environment=<id>
```

FTS5 over the server's own history. It is the complement of the listing endpoints
rather than a variant of them: those answer "what happened in *this directory*", which a
sidebar asks on every render, while this answers a question a user has to already know
how to phrase.

**It covers everything the server holds.** The index is over the same rows the timeline
renders, including sessions read out of an agent's native files, so a client may present it
as "search my history" without qualification — the coverage is a contract rather than a
function of what some parser reached (`docs/architecture.md` § One authority).

**It never returns 503**, and answers `200` with an empty `results` array for a query
that finds nothing. Whitespace-only `q` is treated as absent and answers empty without
touching the index, so a client may bind the field directly to keystrokes.

`limit` defaults to 20. There is no pagination and deliberately no cursor: a hit is a
jumping-off point to a transcript, so the useful control is a narrower query, not a
deeper page.

**What a hit contains**, since a session is now identified by a triple and there is no
second store to name:

```json
{
  "results": [
    {
      "session": { "id": "env7fk2:claude:9bdacb6e-…", "workdir": "…", "…": "…" },
      "bestMatch": {"seq": 412, "turnId": "…", "excerpt": "…the matched text…"},
      "hits": 3
    }
  ]
}
```

`session` is the same object the listing endpoints return, so a client renders one row
component for both and never has a shape that exists only in search. `bestMatch` carries
the `seq` a client scrolls to and the excerpt that is the only text on the row containing
the query — which is why the search overlay keeps its own ranking rather than being
re-scored by a client-side list engine (`docs/frontend-architecture.md`). `hits` is the
count within that session, so one strong match and thirty weak ones are distinguishable.

Both `bestMatch` and `hits` are omitted rather than zeroed when the index cannot supply
them; a client must not read an absent `bestMatch` as "matched at the beginning".

**This is the one read that must stay off any polled path.** Every other operation
against this store is microseconds to sub-millisecond; a full-text query is two to three
orders of magnitude slower, and slower still under concurrent write load. FTS5 work is
real work and nothing about owning the store makes it cheaper, so the cost model that
governs every other read does not transfer here — **this endpoint is user-initiated, and a
client gates it on a real query rather than binding it to keystrokes.**

### Files

Browsing and managing a session's working directory, for the Artifacts pane. Reads
are unrestricted within the root. Writes are three narrow endpoints — create a
file (`POST .../files`), create a directory (`POST .../files/directory`), delete
one entry (`DELETE .../files`) — and each refuses its destructive default: an
upload will not overwrite unless asked, a mkdir will not replace, and a delete
will not recurse unless asked and will not touch the root at all.

There is still deliberately no rename and no write-in-place: the agent mutates
file *contents*, the browser manages the files around them.

**Why delete exists at all.** The pane is the only view of the workspace a user has
while a session runs, and a file browser that cannot remove the file it just showed you
pushes the user to a terminal. It is acceptable because the guards are enforced
server-side rather than as UI conventions — see the endpoint below — and the cost is worth
stating plainly: this is the one irreversible operation in the API, because there is no
trash to restore from.

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

Errors: `FILE_NOT_FOUND` (404), `POLICY_VIOLATION` (403) for the root or an escape,
`INVALID_REQUEST` (400) for a non-empty directory without `recursive`. Note the code is
`POLICY_VIOLATION`, not `FORBIDDEN` — that is the HTTP status name, and a client
branching on it never matches (`Error::Forbidden` → `POLICY_VIOLATION`,
`backend/src/error.rs:229`).

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
the bound cannot come from the request. It comes from `SYLPH_BROWSE_ROOTS`
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

Full machine-readable spec: `docs/api-v1.openapi.json`, which is authoritative for the
wire contract.

**Verify coverage by diffing, never by trusting a count.** A route added to
`backend/src/lib.rs` without a matching path here is the recurring failure, and the check
is mechanical: normalize path parameters and diff the spec against the `.route(...)` calls,
in both directions. Counts stated in prose have been wrong here in both directions at once
— claiming full coverage while a whole control plane was undocumented, and naming
endpoints as gaps that were not. **A claim that something is documented is itself a thing
to verify**, so grep for the heading too.

Two defects worth knowing about, because a path-level count cannot see either:

- **A response that `$ref`s the *request* schema.** A generated client then types the
  response as the body it just sent. A path being present is not a path being right, so
  check that each response names a *response* shape.
- **A response declaring no content at all**, which a generator reads as an untyped body
  rather than as an omission.

`error::tests::codes_match_the_openapi_enum` is the one part of this contract a failure
currently reports on its own; extending that to path coverage is a standing suggestion in
`docs/roadmap.md`.

The frontend does not consume generated types: `frontend/lib/api.ts` is hand-written
and mirrors the wire shape directly. Generating a second definition of every shape
would mean two sources to keep in step; see `docs/frontend-architecture.md` § Types. The
backend does pin one thing to this file automatically —
`error::tests::codes_match_the_openapi_enum` fails if an error code is emitted that the
schema's enum does not publish.
