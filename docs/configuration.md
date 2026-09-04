# Configuration

Sylph keeps everything it owns under **one root**, `~/.sylph`, and takes its
settings from **one file**, `~/.sylph/config.toml`. Nothing outside that root is
read for configuration, discovery, or identity.

## Why one root

One root and one file, rather than a set of independent environment variables each with
its own default, because four things follow from it and each is otherwise a recurring cost:

- **It is discoverable.** There is a directory to look in and a file to open.
- **A deployment sets one variable.** launchd and other supervisors start from a bare
  environment, so anything not set explicitly is absent — and a variable *added later* is
  then silently missing from every existing install.
- **A second instance is one override**, not an incantation.
- **Uninstall is one path**, because one path describes everything the app owns.

## Layout

```
~/.sylph/                Server. One root, one file.
├── config.toml          Settings. Hand-edited. Seeded fully commented on first run.
├── SOUL.md              The identity preamble. Yours to edit; seeded from the bundle.
├── data/                Everything durable. This is what to back up.
│   ├── journal.db       The journal: sessions, turns, in-flight events.
│   ├── history.db       Consolidated history, and the FTS index over it.
│   ├── blobs/           Content-addressed payloads, referenced by history.db.
│   └── attachments/     Files staged for a turn, by session and turn id.
├── scratch/             Where Quick Start sessions run.
└── logs/                Server output, when started by a supervisor.

~/.sylph-env/            Envagent. Separate root, because it is a separate product.
├── config.toml          Its own settings: which server, what it may run.
├── credential           Written by `pair`. Not hand-edited.
└── spool.db             Events not yet acked by the server. Bounded; ack-gated.
```

Three properties of that shape are deliberate.

**`data/` is one subtree, so backup and migration are one path.** `journal.db`,
`history.db`, and `blobs/` are one store split for SQLite's sake, and
`attachments/` is referenced by turns inside it. Copying `data/` copies a
consistent whole; copying any part of it does not.

**There is no `agents/` directory.** Adapters are linked into the envagent binary
(`docs/session-protocol.md`), so there is nothing to drop into one. The case such a
directory would serve — "a vendor changed faster than the shipped adapter" — is answered by
upgrading the **envagent on that one machine**, which touches no server, no schema, and no
other environment. A drop-in directory also carries a hazard of its own: a copied-in
adapter is frozen at the version that was copied, and silently outlives the upgrade that
should have replaced it.

**The two roots are separate even on one machine.** A server does not embed an
envagent (`docs/environments.md`), so "the machine the server runs on" has both roots
and no special code path. Sharing one root would make that machine the only
environment configured differently from every other, which is the kind of special case
that drifts.

**`scratch/` is a real directory, not `$HOME`.** Quick Start exists so you can
ask something without choosing a project, which still requires a working
directory. Pointing it at `$HOME` gives an agent the whole home directory as its
workspace and files it creates land wherever it chose; a directory the app owns
keeps them findable and keeps the sidebar's Quick Sessions group meaningful.

## Precedence

Every setting resolves the same way, most specific first:

1. **Environment variable** — for a supervisor, a test, or one-off run.
2. **`config.toml`** — what a user sets and expects to persist.
3. **Built-in default** — derived from the root wherever a path is involved.

Environment wins because that is what makes a throwaway instance and an
overridden test possible without editing a user's file. The config file wins over
the default because that is the whole point of it. Nothing is read from anywhere
else, and no legacy location is consulted.

## Settings

### Server

| `config.toml` | Environment | Default |
| --- | --- | --- |
| — | `SYLPH_HOME` | `~/.sylph` |
| — | `SYLPH_CONFIG` | `<home>/config.toml` |
| `server.host` | `SYLPH_HOST` | `127.0.0.1` |
| `server.port` | `SYLPH_PORT` | `4317` |
| — | `SYLPH_ENV_LISTEN` | *(none; opt-in loopback envagent ingress)* |
| `paths.data_dir` | `SYLPH_DATA_DIR` | `<home>/data` |
| `paths.soul` | `SYLPH_SOUL` | `<home>/SOUL.md` |
| `paths.quick_start_dir` | `SYLPH_QUICK_START_DIR` | `<home>/scratch` |
| `log.level` | `SYLPH_LOG`, then `RUST_LOG` | `info` |

`server.host` binds what it is asked for and **warns loudly when that is not loopback**,
because a non-loopback bind requires an authentication change this system has not made
(`docs/architecture.md` § Security model).

**Either honour a setting or do not accept it.** A value read into config and then ignored
is worse than one not offered: it reports success, changes nothing, and gives an operator a
lever they believe in.

### Envagent

| `config.toml` | Environment | Default |
| --- | --- | --- |
| — | `SYLPH_ENV_HOME` | `~/.sylph-env` |
| `server.url` | `SYLPH_ENV_SERVER` | *(none; overrides the paired wire endpoint; `pair` derives HTTP-port + 1)* |
| `paths.browse_roots` | `SYLPH_BROWSE_ROOTS` | *(none; `$HOME` always included)* |
| `spool.max_bytes` | `SYLPH_ENV_SPOOL_MAX` | `512 MiB` |
| `log.level` | `SYLPH_ENV_LOG`, then `RUST_LOG` | `info` |

**`browse_roots` moved from the server to the envagent, and that is a correctness
change rather than a relocation.** The paths it names exist on the machine running the
agents, so only that machine can canonicalize them, check containment, and know
whether a component is a symlink. A server evaluating a remote path would be checking
a string against a filesystem it cannot see.

The Quick Start directory is a server setting because it is where the *product's*
no-workspace sessions run. When Quick Start targets a remote environment, the server
sends the request and the envagent resolves its own equivalent.

**`spool.max_bytes` bounds the buffer, and overflow stops the agent rather than
dropping events.** A silent gap in the record is worse than a stopped turn
(`docs/environments.md` § Spooling).

### `browse_roots` bounds where agents run, not only what is listed

This is the highest-consequence setting an envagent has, and its name understates it.
`paths.browse_roots` bounds two things:

| Endpoint | Capability |
| --- | --- |
| `GET /api/v1/fs/*` | which directories a client may **enumerate** |
| `POST /api/v1/sessions`, `POST /api/v1/sessions/adopt` | which directories a client may name as a session's **working directory** |

The second is where an agent process is spawned, and a coding agent reads and
writes the tree it is pointed at. So widening this setting widens where arbitrary
code may run, not merely what names are visible.

Until this bound existed, `POST /sessions` checked only that the path existed —
so any client of this backend could start an agent anywhere on the filesystem
while the *narrower* capability, listing directory names, was bounded. The
narrower one being bounded is what made this easy to miss.

**One key rather than two**, and not for brevity. `/api/v1/fs/*` exists for exactly one
purpose — naming a workdir — so a second key would have to be kept equal to this one for the
picker to stay honest, and **two keys that must agree are two keys that will not.** Widening
one alone yields either a picker offering directories creation refuses, or, far worse,
creation accepting directories the operator only meant to be listable. One key means one
blast radius, stated once.

The Quick Start directory is permitted in addition, always, because it is where
the product's own no-workspace sessions run and it is only *coincidentally*
browsable: both `SYLPH_HOME` and `paths.quick_start_dir` can move it outside
`$HOME`, and an install that did so would otherwise find Quick Start refused by a
bound nobody aimed at it.

**What the check is.** Canonicalize, then require containment — the same
`backend/src/paths.rs` primitive both file APIs use. Canonicalizing is what
defeats `..` and symlinks, including a symlink that lives on disk rather than in
the request, which no amount of inspecting the request string can see. The
canonical path is what gets journalled, so two spellings of one directory
(`/tmp/x` and `/private/tmp/x` on macOS) cannot become two workspaces, and the
recorded path has no symlinked component that could be re-pointed after the check.

**Refusals name the bound, not the path.** A directory that does not exist is
`400 INVALID_REQUEST`; one that exists outside every root is `403
POLICY_VIOLATION` listing what is permitted. The two send a reader to different
places — the filesystem, or this file — and reporting the second as "does not
exist" would send them to the wrong one.

**Narrowing this setting does not orphan existing sessions.** Nothing re-checks a
journalled workdir when a session is read, listed, or resumed. The bound applies
where a path enters as a new decision; a rule that applied on read would make
every session created under wider configuration unreadable, which is a worse
failure than the hole it closes. Reading a session's *files* stays bounded either
way, by that session's own workdir (`backend/src/api/files.rs`).

`paths.soul` has a third state the others do not. An **empty** value — `soul = ""`
or `SYLPH_SOUL=""` — means *never inject identity*, which the adapter protocol
reserves and which is different from leaving it unset: unset resolves the default
path, empty declines outright and nothing is seeded there. A configured path that
does not exist also resolves to no identity, matching the adapters' own
`[[ -f "$SOUL_PATH" ]] || SOUL_PATH=""`.

Paths accept `~` and `~/…`, because a hand-edited config that rejected the
spelling a user would naturally type would be a worse file to hand-edit.

Which agents an environment can run is not a setting at all: it is whether the vendor
CLI is on that machine's `PATH`, reported by `sylph-envagent doctor` and by
`/api/v1/health` per environment. `SYLPH_BUNDLED_AGENTS_DIR` is gone with the adapter
directory it named.

## First run

Starting with no `config.toml` creates the root, writes a **fully commented**
`config.toml`, and seeds `SOUL.md` from the copy compiled into the binary.

The seeded config sets nothing. Every key is commented out with its default
written beside it, which is what makes the file a place to learn what is
configurable rather than a second copy of the defaults that can drift out of
agreement with them.

`SOUL.md` is seeded only on first run, never restored afterwards. Deleting it is
a meaningful act — the protocol reserves "no identity" — and re-creating a file a
user removed would be the app arguing with them. `sylph-web-backend config init`
puts it back on request.

## Inspecting what resolved

```sh
sylph-web-backend doctor
```

reports every resolved path, whether it exists, and which source supplied it.
`sylph-envagent doctor` does the same for an envagent, and adds which agent CLIs it
found with their versions and which server it is paired to. It exists because the failures in this area are all of one shape: a
misconfigured path produces a server that starts, reports healthy, and cannot do
its job — which reads as a UI bug three layers from the cause.

`GET /api/v1/config` returns the same resolved values to the settings panel.
It is **read-only**: the values name filesystem paths, and `browse_roots` bounds
what any client of this backend may enumerate, so a loopback HTTP surface that
could rewrite it would be a materially larger security question than one that
displays it. The panel shows the config file's path so the user knows what to
open.

## Moving an existing install

```sh
sylph-web-backend migrate --from <old-data-dir>
```

copies a journal, history, blob store, and attachments into `~/.sylph/data`,
refusing rather than overwriting if the destination already holds a journal. It
takes an explicit path: nothing in the running server knows about any previous
location, which is what keeps the old layout from surviving as a fallback nobody
maintains.

An install that lived under `~/.agents/data/sylph/web` migrates with:

```sh
sylph-web-backend migrate --from ~/.agents/data/sylph/web
```

## What is not here

UI preferences — theme, density, keyboard bindings, which agent a new session
defaults to — live in the browser's `localStorage`, not in this file. They are
display state with no effect on what the server does, they must be readable
before first paint, and storing them here would mean a round trip before the
first render and a config write on every toggle. That trade changes if Sylph ever
serves more than one user, which is already gated behind a separate
authentication change.
