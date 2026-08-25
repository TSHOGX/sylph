# Configuration

Sylph keeps everything it owns under **one root**, `~/.sylph`, and takes its
settings from **one file**, `~/.sylph/config.toml`. Nothing outside that root is
read for configuration, discovery, or identity.

## Why one root

Before this, six independent environment variables each carried their own
`$HOME`-relative default, pointing into `~/.agents` — a directory belonging to a
different project. That had four costs, and each one was paid more than once:

- **Nothing was discoverable.** A new user had no directory to look in, no file
  to open, and no way to learn that `TRACEDB_BIN` existed.
- **Every deployment had to enumerate them.** launchd starts from a bare
  environment, so the installer wrote each variable it happened to see into the
  plist. A variable added later was silently absent from existing installs.
- **A second instance was a five-variable incantation** rather than one.
- **Uninstall could not be complete**, because no single path described what the
  app owned.

One root fixes all four at once: `SYLPH_HOME` is the only variable a deployment
must set, the config file is the thing to open, a second instance is one
override, and uninstall is one directory.

## Layout

```
~/.sylph/
├── config.toml          Settings. Hand-edited. Seeded fully commented on first run.
├── SOUL.md              The identity preamble. Yours to edit; seeded from the bundle.
├── agents/              Agent adapters you added. Each is <name>/adapter.sh.
├── data/                Everything durable. This is what to back up.
│   ├── live.db          The journal: sessions, turns, in-flight events.
│   ├── history.db       Consolidated history.
│   ├── blobs/           Content-addressed payloads, referenced by history.db.
│   └── attachments/     Files staged for a turn, by session and turn id.
├── scratch/             Where Quick Start sessions run.
└── logs/                Server output, when started by a supervisor.
```

Three properties of that shape are deliberate.

**`data/` is one subtree, so backup and migration are one path.** `live.db`,
`history.db`, and `blobs/` are one store split for SQLite's sake, and
`attachments/` is referenced by turns inside it. Copying `data/` copies a
consistent whole; copying any part of it does not.

**`agents/` is not where bundled adapters live.** An adapter is versioned with
the protocol it implements, so it ships beside the binary and is replaced on
upgrade. This directory is the user's own set, searched *after* the bundled ones
so a name here shadows a bundled adapter — the escape hatch when a vendor
changes something faster than the shipped adapter catches up. Copying bundled
adapters into here would freeze them at install time and break the next upgrade.

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

| `config.toml` | Environment | Default |
| --- | --- | --- |
| — | `SYLPH_HOME` | `~/.sylph` |
| — | `SYLPH_CONFIG` | `<home>/config.toml` |
| `server.host` | `SYLPH_HOST` | `127.0.0.1` |
| `server.port` | `SYLPH_PORT` | `4317` |
| `paths.data_dir` | `SYLPH_DATA_DIR` | `<home>/data` |
| `paths.agents_dir` | `SYLPH_AGENTS_DIR` | `<home>/agents` |
| `paths.soul` | `SYLPH_SOUL` | `<home>/SOUL.md` |
| `paths.quick_start_dir` | `SYLPH_QUICK_START_DIR` | `<home>/scratch` |
| `paths.browse_roots` | `SYLPH_BROWSE_ROOTS` | *(none; `$HOME` is always included)* |
| `tracedb.bin` | `SYLPH_TRACEDB_BIN` | `trace-db` on `PATH` |
| `log.level` | `SYLPH_LOG`, then `RUST_LOG` | `info` |

`SYLPH_BROWSE_ROOTS` is colon-separated, matching `PATH`. In the file it is a
list of strings.

Paths accept `~` and `~/…`, because a hand-edited config that rejected the
spelling a user would naturally type would be a worse file to hand-edit.

The bundled adapter directory is resolved separately and is not a setting: it is
found beside the binary, since that is the only location that stays correct when
an install tree is moved or copied. See `doctor` below for what it resolved.

## First run

Starting with no `config.toml` creates the root, writes a **fully commented**
`config.toml`, and seeds `SOUL.md` from the copy compiled into the binary.

The seeded config sets nothing. Every key is commented out with its default
written beside it, which is what makes the file a place to learn what is
configurable rather than a second copy of the defaults that can drift out of
agreement with them.

`SOUL.md` is seeded only on first run, never restored afterwards. Deleting it is
a meaningful act — the protocol reserves "no identity" — and re-creating a file a
user removed would be the app arguing with them. `sylph-web config init --soul`
puts it back on request.

## TraceDB

TraceDB is a separate project that Sylph depends on for durable cross-agent
history. It is found on `PATH` by default, which is where a normal install of it
lands, and can be pinned with `tracedb.bin` or `SYLPH_TRACEDB_BIN` for a
checkout-local build:

```toml
[tracedb]
bin = "~/Workspace/trace-db/target/release/trace-db"
```

An unreachable TraceDB is not a failure. Live sessions work; archive search and
native import report themselves unavailable, and `/api/v1/health` says
`tracedb.available: false`.

## Inspecting what resolved

```sh
sylph-web doctor
```

reports every resolved path, whether it exists, which one the config file
supplied, how many agents are usable and why any are not, and whether TraceDB
answers. It exists because the failures in this area are all of one shape: a
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
sylph-web migrate --from <old-data-dir>
```

copies a journal, history, blob store, and attachments into `~/.sylph/data`,
refusing rather than overwriting if the destination already holds a journal. It
takes an explicit path: nothing in the running server knows about any previous
location, which is what keeps the old layout from surviving as a fallback nobody
maintains.

An install that lived under `~/.agents/data/sylph/web` migrates with:

```sh
sylph-web migrate --from ~/.agents/data/sylph/web
```

## What is not here

UI preferences — theme, density, keyboard bindings, which agent a new session
defaults to — live in the browser's `localStorage`, not in this file. They are
display state with no effect on what the server does, they must be readable
before first paint, and storing them here would mean a round trip before the
first render and a config write on every toggle. That trade changes if Sylph ever
serves more than one user, which is already gated behind a separate
authentication change.
