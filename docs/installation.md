# Installation

Installing Sylph for daily use. To run it from a checkout while developing, see the Quick
Start in `README.md`.

**Two products install separately: `sylph-server` and `sylph-envagent`.** The server holds
every session's record and all policy; an envagent runs on each machine where agents
execute and dials back to the server. On a single machine you install both, in one
command, and you still get two processes with two roots — the server does not embed an
envagent (`docs/environments.md`).

The server binds to loopback and refuses anything else, because exposing it requires an
authentication and threat-model change first (`docs/architecture.md` § Security model).
The envagent transport holds to the same rule. Who may authorize a Subject on an envagent
is settled by [ADR-0006](adr/0006-environment-authority-and-capability-grants.md).

## Requirements

- macOS (the LaunchAgent path is macOS-specific; the rest is portable)
- Rust 1.89+ and Node.js 22.5+, needed to build — not to run
- The agent CLIs you want — `claude`, `codex`, `pi` — on the `PATH` of each machine
  running an **envagent**, not on the server's

## Install

```sh
git clone <repo> sylph-web
cd sylph-web
./scripts/install.sh
```

That builds the release server, the envagent, and a standalone frontend bundle, stages
them under a prefix, and links launchers onto your PATH. Nothing needs the checkout
afterwards — the bundle carries its own `node_modules`.

Start it:

```sh
sylph-web            # runs the server and the frontend; Ctrl-C stops both
```

Then pair this machine, which is what makes it able to run anything:

```sh
sylph-server pair --new                 # prints a short-lived code
sylph-envagent pair --code <code>
```

Then open http://localhost:3000.

To start at login instead:

```sh
./scripts/install.sh --launchagent
```

The installer waits for `/api/v1/health` and fails loudly if the server does not come up,
so a broken install does not look like a successful one.

## Paths and settings

The server keeps what it owns under `~/.sylph` and an envagent under `~/.sylph-env`, both
created on first start. [**configuration.md**](configuration.md) owns the settings table
and the port allocation.

The installer itself reads only `SYLPH_HOME`, `SYLPH_PORT`, `SYLPH_WEB_FRONTEND_PORT`,
`SYLPH_WEB_PREFIX`, and `SYLPH_WEB_BIN_DIR`. Set them for the install command rather than
only at start time:

```sh
SYLPH_PORT=4400 SYLPH_WEB_FRONTEND_PORT=3400 ./scripts/install.sh
```

The server port has to be set at build time, because Next compiles its proxy target into
the route manifest and a port supplied only when starting is ignored. The installer
handles this; it matters if you build by hand.

Everything else — the data directory, the identity preamble, the Quick Start directory,
log level — is set in `config.toml`, so the LaunchAgent plist carries `SYLPH_HOME` and
does not grow a key every time a setting is added.

## Your data

```
~/.sylph/
├── config.toml      Your settings. Edit freely; commented on creation.
├── SOUL.md          The identity preamble. Yours to edit or delete.
├── data/            The journal, history, blobs, attachments. Back this up.
└── scratch/         Where Quick Start sessions run.

~/.sylph-env/
├── config.toml      Which server, and where this machine may run agents.
├── credential       Written by `pair`. Not hand-edited.
└── spool.db         Events not yet acked by the server. Bounded, ack-gated.
```

`data/` is the one directory that matters for backup, and it is one subtree on purpose:
the two SQLite tiers, the blob store, and staged attachments reference each other, so
copying `data/` copies a consistent whole.

`spool.db` is why closing a laptop mid-turn does not put a hole in the record: events are
written there before being sent and deleted only when the server acks past them
(`docs/environments.md` § Spooling). It is authoritative for nothing.

`browse_roots` lives in the envagent's config because only this machine can canonicalize
its own paths and see whether a component is a symlink. It bounds where an agent may run,
so widening it widens where arbitrary code executes.

The installer touches only `data/`, which survives upgrades and uninstalls. `--purge`
deletes the whole root, which is the one operation that needs asking for.

To check what resolved:

```sh
sylph-server doctor
sylph-envagent doctor
```

**Nothing else to install, and no database to configure.** History is one store the server
owns under `data/`, holding both what Sylph ran and what it read out of an agent's native
files (`docs/architecture.md` § One authority). There is no adapter directory: adapters
are compiled into the envagent, so a vendor that changes faster than the shipped adapter
is answered by upgrading the envagent on that one machine.

An agent's own native files are never written by anything here. Vendors may prune those
files independently, so Sylph's retained raw records and blobs can be the only surviving
copy. Preserve the complete server data directory when upgrading.

## Upgrading

Pull and re-run the installer. It is idempotent, and it keeps the previous version
alongside the new one so `--rollback` restores it.

```sh
git pull
./scripts/install.sh          # add --launchagent if you use one
```

Sylph is in development and ships no schema migrations. A build that cannot read an
existing store refuses to start and names the store. Stop both processes and make a copy
of the complete data directory before changing schemas. Never delete a retained raw
archive on the assumption that a vendor still holds it. For a compatible raw schema,
rebuild session projections through `POST /api/v1/sessions/{id}/reproject`; this requires
the owning envagent but no vendor source files. Keep the prior binary and data copy
together until the rebuilt transcript has been verified. Test data may be discarded
explicitly and re-adopted when its vendor sources still exist. Upgrade an envagent
independently of the server; it is a separate product on a separate root.

## Logs and health

With a LaunchAgent, output goes to `$SYLPH_WEB_PREFIX/logs/`. Each file is rotated at
startup once it passes 16 MB, keeping one previous generation.

Rotation copies and then truncates in place rather than renaming, because launchd opens
`StandardOutPath` and `StandardErrorPath` before exec'ing the launcher: renaming would
send the current run's output into the rotated file. launchd also starts from a bare
environment providing only `HOME`, so every other override is written into the plist for
whatever was set when the installer ran.

```sh
tail -f ~/.local/share/sylph-web/logs/stderr.log
```

Two endpoints report state:

```sh
curl -s localhost:4317/api/v1/health        # liveness, no auth needed
curl -s localhost:4317/api/v1/diagnostics   # counts and uptime, needs a cookie
```

`/health` reports each paired environment with the agents and versions it found. It stays
unauthenticated so a supervisor can probe it, and therefore reports no counts or paths.
`/diagnostics` adds session and turn counts, uptime, and the last retention pass.

Check the agent itself with:

```sh
launchctl list | grep dev.sylph.web
```

## Uninstalling

```sh
./scripts/install.sh --uninstall     # removes the install, keeps ~/.sylph
./scripts/install.sh --purge         # also deletes ~/.sylph
```

`--purge` removes your config, identity file, sessions, and history, which is why it is a
separate flag.

## Troubleshooting

Start with `sylph-server doctor` and `sylph-envagent doctor`. Between them they print
every resolved path, which source supplied it, whether it exists, which agent CLIs were
found at which versions, and which server the envagent is paired to. Settings →
Configuration shows the server's half in the UI.

**Port already in use.** The installer stops its own agent before checking, so this means
another process holds the port. Find it with `lsof -ti :4317 -sTCP:LISTEN`, or install on
a different port. A server owns two adjacent ports (`docs/configuration.md` § Ports), so
check the one above it too.

**No agents to pick from.** Either no envagent is connected, or the one that is has no
agent CLI on its `PATH` at a version the adapter accepts. `GET /api/v1/health` reports
each environment with what it found, and the three fixes differ: start or pair the
envagent, install the CLI on that machine, or upgrade it.

**A session says it cannot be driven.** Two states, distinguished by the link's typed
failure. `environment_unreachable` means that machine is not connected, so history still
reads but nothing can be sent. `not_ours_to_drive` means the process is alive under
someone else and no control transport reaches it — `pi` never offers one, and an adopted
`claude` session has no channel for interrupt or steer (`docs/agents.md`). Neither is
fixed by retrying.

**A setting in `config.toml` seems to be ignored.** An environment variable overrides the
file, and `doctor` names the source of every value.

**One half is up and the other is not.** The launcher exits when either process dies, so
the LaunchAgent restarts the pair. If it recurs, `logs/stderr.log` has the reason.

**The agent will not stay loaded.** `launchctl list` shows a non-zero exit status. The
usual cause is a `PATH` without `node`; the installer writes the `node` it built with into
the plist, so a moved Node install needs a re-run.
