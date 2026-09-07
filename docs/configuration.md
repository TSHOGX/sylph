# Configuration

There are two products, so there are two roots and two files. The server keeps
everything it owns under `~/.sylph` and reads `~/.sylph/config.toml`; an envagent
keeps its own under `~/.sylph-env`. Nothing outside a root is read for
configuration, discovery, or identity.

## Why one root per product

One root and one file, rather than independent variables each with its own default:

- There is a directory to look in and a file to open.
- A deployment sets one variable. Supervisors start from a bare environment, so a
  setting added later still resolves from the root that was already configured.
- A second instance is one override.
- Uninstall is one path, because one path describes everything the product owns.

The roots stay separate on a single machine, because the server does not embed an
envagent (`docs/environments.md`). "The machine the server runs on" therefore has both
roots and no privileged code path.

## Layout

```
~/.sylph/                Server.
├── config.toml          Settings. Hand-edited. Seeded fully commented on first run.
├── SOUL.md              The identity preamble. Yours to edit; seeded from the bundle.
├── data/                Everything durable. This is what to back up.
│   ├── journal.db       The journal: sessions, turns, in-flight events.
│   ├── history.db       Consolidated history, and the FTS index over it.
│   ├── blobs/           Content-addressed payloads, referenced by history.db.
│   └── attachments/     Files staged for a turn, by session and turn id.
├── scratch/             Where Quick Start sessions run.
└── logs/                Server output, when started by a supervisor.

~/.sylph-env/            Envagent.
├── config.toml          Its own settings: which server, what it may run.
├── credential           Written by `pair`. Not hand-edited.
└── spool.db             Events not yet acked by the server. Bounded; ack-gated.
```

`data/` is one subtree so backup is one path: `journal.db`, `history.db`, and `blobs/`
are one store split for SQLite's sake, and `attachments/` is referenced by turns inside
it. Copying `data/` copies a consistent whole.

Adapters are linked into the envagent binary, so there is no adapter directory to
configure. A vendor that changes faster than the shipped adapter is answered by
upgrading the envagent on that one machine, which touches no server, no schema, and no
other environment.

`scratch/` is a real directory rather than `$HOME`. Quick Start exists so a user can ask
something without choosing a project, which still needs a working directory; a directory
the product owns keeps what an agent creates findable and keeps the sidebar's Quick
Sessions group meaningful.

## Precedence

Every setting resolves the same way, most specific first:

1. **Environment variable** — for a supervisor, a test, or a one-off run.
2. **`config.toml`** — what a user sets and expects to persist.
3. **Built-in default** — derived from the root wherever a path is involved.

Environment wins so a throwaway instance and an overridden test need no edit to a user's
file. The file wins over the default because that is what the file is for.

**Either honour a setting or do not accept it.** A value read into config and then
ignored reports success, changes nothing, and gives an operator a lever they believe in.

## Ports

A server instance owns **two adjacent ports**: an HTTP port for clients, and the port
above it for envagent ingress. One number therefore configures an instance, and the two
cannot be set inconsistently.

| Instance | HTTP | Envagent ingress | Frontend |
| --- | --- | --- | --- |
| default | 4317 | 4318 | 3000 |
| E2E suite | 4327 | 4328 | 3101 |

**Instances are spaced ten apart**, which leaves room for the derived port and keeps a
suite clear of a development server. `SYLPH_E2E_PORT_OFFSET` shifts the suite's whole
allocation for a second checkout and must be a multiple of ten; it moves the HTTP and
frontend ports together. `scripts/measure-bundle.sh` serves a production bundle on 3199,
outside both allocations because it starts no backend.

`SYLPH_ENV_SERVER` on an envagent names the ingress port, not the HTTP one. `pair`
derives it, so a hand-set value is the only way the two can disagree.

## Settings

### Server

| `config.toml` | Environment | Default |
| --- | --- | --- |
| — | `SYLPH_HOME` | `~/.sylph` |
| — | `SYLPH_CONFIG` | `<home>/config.toml` |
| `server.host` | `SYLPH_HOST` | `127.0.0.1` |
| `server.port` | `SYLPH_PORT` | `4317` |
| `server.env_port` | `SYLPH_ENV_LISTEN` | `<port> + 1` |
| `paths.data_dir` | `SYLPH_DATA_DIR` | `<home>/data` |
| `paths.soul` | `SYLPH_SOUL` | `<home>/SOUL.md` |
| `paths.quick_start_dir` | `SYLPH_QUICK_START_DIR` | `<home>/scratch` |
| `log.level` | `SYLPH_LOG`, then `RUST_LOG` | `info` |

`server.host` binds what it is asked for and refuses a non-loopback address, because a
network-reachable bind requires the authentication change described in
`docs/architecture.md` § Security model. The envagent ingress holds to the same rule and
resolves relative to `server.port`, so both listeners move together.

`paths.soul` has a third state the other paths do not. An **empty** value — `soul = ""`
or `SYLPH_SOUL=""` — means never inject identity, which the adapter protocol reserves
and which differs from leaving it unset: unset resolves the default path. A configured
path that does not exist resolves to no identity as well, matching the adapters' own
`[[ -f "$SOUL_PATH" ]] || SOUL_PATH=""`.

### Envagent

| `config.toml` | Environment | Default |
| --- | --- | --- |
| — | `SYLPH_ENV_HOME` | `~/.sylph-env` |
| `server.url` | `SYLPH_ENV_SERVER` | *(written by `pair`)* |
| `paths.browse_roots` | `SYLPH_BROWSE_ROOTS` | *(none; `$HOME` always included)* |
| `spool.max_bytes` | `SYLPH_ENV_SPOOL_MAX` | `512 MiB` |
| `log.level` | `SYLPH_ENV_LOG`, then `RUST_LOG` | `info` |

`browse_roots` belongs to the envagent because the paths it names exist on the machine
running the agents. Only that machine can canonicalize them, check containment, and see
whether a component is a symlink.

`spool.max_bytes` bounds the buffer, and overflow stops the agent rather than dropping
events: a gap in the record is worse than a stopped turn (`docs/environments.md`
§ Spooling).

Quick Start is a server setting because it is where the *product's* no-workspace sessions
run. Targeting a remote environment sends the request onward and the envagent resolves
its own equivalent.

Which agents an environment can run is not a setting. It is what the envagent finds on
that machine, at the version it finds — reported by `sylph-envagent doctor` and, per
environment, by `GET /api/v1/environments` (`docs/environments.md` § Agent availability).

### Installer

`scripts/install.sh` reads its own variables, which name where artifacts land rather than
how a running product behaves:

| Environment | Default |
| --- | --- |
| `SYLPH_WEB_PREFIX` | `~/.local/share/sylph-web` |
| `SYLPH_WEB_BIN_DIR` | `~/.local/bin` |
| `SYLPH_WEB_FRONTEND_PORT` | `3000` |
| `SYLPH_WEB_LAUNCH_LABEL` | `dev.sylph.web` |

The frontend reads `SYLPH_WEB_BACKEND` (default `http://localhost:4317`) **at build
time**, since Next compiles proxy rewrites into the route manifest, and
`SYLPH_WEB_DEV_ORIGINS` under `next dev` only (`docs/development.md`).

### `browse_roots` bounds where agents run

This is the highest-consequence setting an envagent has, and its name understates it.
`paths.browse_roots` bounds two things:

| Endpoint | Capability |
| --- | --- |
| `GET /api/v1/fs/*` | which directories a client may **enumerate** |
| `POST /api/v1/sessions`, `POST /api/v1/sessions/adopt` | which directories a client may name as a session's **working directory** |

The second is where an agent process is spawned, and a coding agent reads and writes the
tree it is pointed at. Widening this setting therefore widens where arbitrary code may
run, not merely what names are visible.

**One key rather than two.** `/api/v1/fs/*` exists for exactly one purpose — naming a
workdir — so a second key would have to be kept equal to this one for the picker to stay
honest, and two keys that must agree are two keys that will not. One key means one blast
radius, stated once.

The Quick Start directory is permitted in addition, always. It is where the product's own
no-workspace sessions run and is only coincidentally browsable: both `SYLPH_HOME` and
`paths.quick_start_dir` can move it outside `$HOME`.

**What the check is.** Canonicalize, then require containment. Canonicalizing defeats
`..` and symlinks, including a symlink on disk rather than in the request, which no
inspection of the request string can see. The canonical path is what gets journalled, so
two spellings of one directory (`/tmp/x` and `/private/tmp/x` on macOS) cannot become two
workspaces, and the recorded path has no symlinked component that could be re-pointed
after the check.

**Refusals name the bound.** A path that does not exist is `400 INVALID_REQUEST`; one
that exists outside every root is `403 POLICY_VIOLATION` listing what is permitted. The
two send a reader to different places — the filesystem, or this file.

**Narrowing this setting does not orphan existing sessions.** The bound applies where a
path enters as a new decision; nothing re-checks a journalled workdir when a session is
read, listed, or resumed. Reading a session's *files* stays bounded either way, by that
session's own workdir.

Paths accept `~` and `~/…`, because a hand-edited file should accept the spelling a user
would naturally type.

## First run

Starting with no `config.toml` creates the root, writes a **fully commented**
`config.toml`, and seeds `SOUL.md` from the copy compiled into the binary.

The seeded config sets nothing: every key is commented out with its default written
beside it, which makes the file a place to learn what is configurable rather than a
second copy of the defaults that can drift away from them.

`SOUL.md` is seeded only on first run. Deleting it is a meaningful act — the protocol
reserves "no identity" — so a restart leaves it deleted, and `sylph-server config init`
puts it back on request.

## Inspecting what resolved

```sh
sylph-server doctor
```

reports every resolved path, whether it exists, and which source supplied it.
`sylph-envagent doctor` does the same for an envagent, and adds which agent CLIs it found
with their versions and which server it is paired to. Both exist because failures in this
area share one shape: a misconfigured path produces a process that starts, reports
healthy, and cannot do its job.

`GET /api/v1/config` returns the same resolved values to the settings panel, from the
same producer, so the three surfaces cannot disagree. It is **read-only**: the values
name filesystem paths and `browse_roots` bounds where code may run, so a surface that
could rewrite them would be a materially larger security question than one that displays
them. The panel shows the config file's path so a user knows what to open.

## What is not here

UI preferences — theme, density, keyboard bindings, which agent a new session defaults to
— live in the browser's `localStorage`. They are display state with no effect on what the
server does, they must be readable before first paint, and storing them here would mean a
round trip before the first render and a config write on every toggle. That trade changes
only if Sylph serves more than one user, which is gated behind a separate authentication
change.