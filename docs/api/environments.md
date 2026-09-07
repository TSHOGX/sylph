# API — Environments, agents, and identity

Health, diagnostics, the identity preamble, environment and agent discovery, and
workspaces. `docs/api/README.md` holds the conventions every endpoint here assumes.

## Health and diagnostics

### `GET /api/v1/health`

Liveness, version, and what each paired environment can run. Unauthenticated, so a
supervisor can probe it — which is why it carries no counts and no paths.

**Response 200:**
```json
{
  "ok": true,
  "apiVersion": "1",
  "schemaVersion": 7,
  "environments": [
    {"id": "env7fk2", "name": "laptop", "connected": true,
     "agents": [{"name": "claude", "version": "2.1.247"}, {"name": "pi", "version": "0.84.3"}]},
    {"id": "envk9x1", "name": "build-box", "connected": false,
     "disconnectedSinceMs": 1724057000000}
  ]
}
```

**Agent availability is reported per environment and nowhere else.** It is a fact about one
machine's `PATH` at one moment, discovered by that envagent and re-reported when its
inventory changes, so a server-wide list would be a claim no single machine can support —
and it would read as authoritative while being wrong for every environment but one. A
disconnected environment reports no `agents` array rather than an empty one: what it can run
is unknown, not nothing.

`schemaVersion` is `MAX(version)` from the migration runner's own table, never a
hand-maintained key (`docs/database-schema.md` § Schema evolution).

### `GET /api/v1/diagnostics`

Counts, uptime, and the last retention pass. Authenticated, unlike `/health`, because it
carries counts, local filesystem paths, and agent failure messages.

**Response 200:**
```json
{
  "sessions": { "total": 15, "idle": 12, "running": 3 },
  "turns": { "queued": 0, "running": 2, "completed": 143 },
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
    "environmentsReachable": {"env7fk2": true, "envk9x1": false},
    "bytesHeld": 4183740928,
    "sessionsMeasured": 357,
    "bytesGrewBy": 12043776,
    "largestSessions": [
      {
        "sessionId": "env7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
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

`retention` is the last completed pass, or `null` before the first one. Two things about
its shape are load-bearing, both covered in `docs/storage.md`:

- **`withheld` beside `terminal`** is what separates correct behaviour from a broken pass,
  and `withheldReasons` is what makes the difference legible. A terminal session is
  *supposed* to keep everything; a backed session that keeps withholding means no
  environment is confirming it. Counting withheld from the authority's own reported reason
  rather than from an absent bound is what keeps "answered, nothing safe yet" distinct from
  "nobody was asked".
- **The byte fields answer "which session is growing".** The counts cannot: forty small
  withheld sessions and one enormous one report identically. `bytesHeld` is the *logical*
  record across every measured session rather than the footprint on disk, so it exceeds what
  `du` reports — a ranking measure, not a disk gauge. `grewBy` is signed (negative means
  pruning worked) and **absent when a session has been measured only once**, because one
  observation is not a trend.

## Identity

### `GET /api/v1/soul`

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

### `GET /api/v1/config`

What configuration resolved to, and which source supplied each value. The same
report `sylph-server doctor` renders, so the terminal and the settings panel
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

Built per request rather than cached: every `exists` field is a filesystem fact, and a
cached report would keep telling a user a directory is missing after they created it. It is
a handful of `stat` calls against the page cache.

See `docs/configuration.md` for the settings themselves.

## Agents and environments

### `GET /api/v1/environments`

Every paired environment, whether it is connected, and what it can run.

**Response 200:**
```json
{
  "environments": [
    {
      "id": "env7fk2",
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
      "id": "envk9x1",
      "name": "prod-builder",
      "connected": true,
      "origin": "ssh-bootstrapped",
      "reachableVia": "env7fk2",
      "agents": [{"name": "codex", "version": "0.9.1", "flags": {}, "supports": [], "available": true}]
    }
  ]
}
```

**Which agents exist is per environment, and time-varying.** An agent is available when
its CLI is on that machine's `PATH` at a version the adapter accepts; the envagent
discovers that on connect and re-reports it when the machine's inventory changes, so
upgrading a CLI moves this response with no re-pair and no server restart. A CLI that is
present but below the floor reports `available: false` with the version it found, which is
a different fix from installing it.

There is no top-level agent list, because the answer differs per machine and a merged one
would be true of nowhere. A picker names the environment it will run in, so a user learns
that a machine has no `codex` before creating the session rather than after.

**`reachableVia` is not decoration.** A non-null value means this environment is reachable
only while the named one relays for it, so **control dies when that machine leaves**
(`docs/environments.md` § SSH-bootstrapped). Rendering it identically to a
directly-connected environment would promise AFK continuity it cannot keep.

**`flags` is the spawn-time option schema** — what can be chosen before a session
exists. Anything adjustable on a *live* session is a config facet instead, reported by
`GET /sessions/{id}/capabilities`, and a session's real slash-command list arrives there
too because it depends on files the CLI reads at startup. Nothing in the UI hardcodes a
model list, which is why `codex` correctly shows effort with no model selector.

**`supports` is a closed vocabulary that drives the UI directly** — a composer must not
offer interrupt to an agent that cannot interrupt.

### `POST /api/v1/environments/pairings`

Mint a short-lived, single-use pairing code (`docs/environments.md` § Pairing). The
envagent exchanges it for a durable credential and is assigned its opaque id.

**Response 201:** `{"code": "…", "expiresAtMs": 1724054460000}`

### `POST /api/v1/environments/pair`

The envagent exchanges the short-lived code for a durable credential. This
loopback-only operation is exempt from the browser cookie because the envagent
does not have a browser session; the code is consumed atomically and cannot be
reused.

### `DELETE /api/v1/environments/{id}`

Unpair. **Gated by the environment's authority root, not by any grant** — a `machine`
environment requires a confirmation code minted by a local command on that machine, a
`server` one requires an `is_admin` Subject (`docs/api/admin.md` § `administer` is
ungrantable, which is not the same as having no route). A browser session alone never
suffices on a `machine` environment, because unpairing is the one operation that ends
Sylph's reach into someone's computer.

**Every session that environment produced stays readable**, searchable, and listable, since
the server's store is the authority. What changes is the link: `observe` continues to answer
from the store, `control` reports unavailable with `environment_unreachable`, and retention
treats those sessions as terminal because no reachable native source can confirm a bound
(`docs/environments.md` § Unpairing is the terminal operation).

The facet is the right carrier here and the wrong one for revocation, which is worth noting
because the two look alike. Unpairing removes the *environment's* reachability, so it reads
identically for every observer; revoking a grant removes one Subject's authorization and
reads differently per caller
(`docs/architecture.md` § A facet describes the environment, never the caller).

### `DELETE /api/v1/environments/{id}/grants/{subject}`

Revoke a grant. Same gate as unpairing, since issuing and revoking are the same authority.

Three effects, in this order: the row is deleted from `env_grants`; new control from that
Subject is refused; and in-flight vendor work whose `origin_subject` is that Subject leaves
its trajectory `abandoned`, on a turn carrying `close_reason = grant_revoked`
([ADR-0006](../adr/0006-environment-authority-and-capability-grants.md) § Revocation reaches
work already in flight). Work from other Subjects on the same environment is untouched.

The route exists here rather than as a local command because `env_grants` is a server
table, and a local command cannot delete a row it does not hold
(`docs/architecture.md` § One authority). Issuing an invite is the mirror case and goes the
other way for the same reason it is out-of-band at all: a wire message that issues a grant
lets one compromised envagent admit Subjects to itself.

## Workspaces

### `GET /api/v1/workspaces`

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
      "environment": "env7fk2"
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

