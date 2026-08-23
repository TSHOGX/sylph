# Installation

This guide covers installing Sylph Web for daily use. To run it from a checkout
while developing, see the Quick Start in `README.md` instead.

Sylph Web binds to loopback only. It has no remote access story, and exposing it
beyond `127.0.0.1` requires an authentication and threat-model change first.

## Requirements

- macOS (the LaunchAgent path is macOS-specific; the rest is portable)
- Rust 1.85+ and Node.js 22.5+, needed to build — not to run
- TraceDB, if you want archived coding-agent sessions. Sylph Web runs without
  it; the archive simply reports as unavailable.

## Install

```sh
git clone <repo> sylph-web
cd sylph-web
./scripts/install.sh
```

That builds the release backend and a standalone frontend bundle, stages both
under a prefix, and links launchers onto your PATH. Nothing needs the checkout
afterwards — the bundle carries its own `node_modules`.

Start it:

```sh
sylph-web            # runs both halves; Ctrl-C stops both
```

Then open http://localhost:3000.

To start at login instead:

```sh
./scripts/install.sh --launchagent
```

The installer waits for `/api/v1/health` and fails loudly if the backend does
not come up, so a broken install does not look like a successful one.

## Paths and settings

| Variable | Default | What it controls |
| --- | --- | --- |
| `SYLPH_WEB_PREFIX` | `~/.local/share/sylph-web` | Where the install lives |
| `SYLPH_WEB_BIN_DIR` | `~/.local/bin` | Where launchers are linked |
| `SYLPH_WEB_DATA_DIR` | `~/.agents/data/sylph/web` | The journal (`live.db`) |
| `SYLPH_WEB_PORT` | `4317` | Backend port |
| `SYLPH_WEB_FRONTEND_PORT` | `3000` | Frontend port |
| `SYLPH_RUNNERS_DIR` | `~/.agents/skills/sylph/runners` | Runner discovery |
| `SYLPH_SOUL` | `<runners_dir>/../SOUL.md` | The identity preamble. An explicit empty value means no identity, so every session runs on the agent's own |
| `TRACEDB_BIN` | `~/Workspace/trace-db/target/release/trace-db` | TraceDB binary |
| `SYLPH_WEB_BROWSE_ROOTS` | *(none; `$HOME` is implicit)* | Extra directories the workspace picker may browse |
| `RUST_LOG` | `info` | Backend log verbosity |

Set them for the install command, not just at start time:

```sh
SYLPH_WEB_PORT=4400 SYLPH_WEB_FRONTEND_PORT=3400 ./scripts/install.sh
```

The backend port in particular has to be set at build time. Next compiles its
proxy target into the route manifest, so a port supplied only when starting is
ignored and the UI talks to `4317` regardless. The installer handles this; it is
worth knowing if you build by hand.

Overrides you set are written into the LaunchAgent plist, because launchd starts
processes with a bare environment and would not otherwise see them.

## Your data

The journal at `$SYLPH_WEB_DATA_DIR/live.db` holds sessions and turns created
through the web app. It is the only state the installer touches, and it survives
upgrades and uninstalls.

Schema changes are applied on startup and recorded in the database, so a
development journal carries into a packaged install with no separate migration
step — the default data directory is the same one `cargo run` uses.

The TraceDB archive is never written and never read directly. Sylph Web talks to
TraceDB over its JSON API, so archived history is unaffected by anything here.

## Upgrading

Pull and re-run the installer. It is idempotent.

```sh
git pull
./scripts/install.sh          # add --launchagent if you use one
```

The previous version is kept alongside the new one, together with a copy of the
journal taken before the new version could migrate it.

## Rolling back

```sh
./scripts/install.sh --rollback
```

This restores the previous version and the journal as it was when that version
last ran. Both steps matter: an older binary refuses to open a journal whose
schema is ahead of it, so reverting code alone would leave it unable to start.

The operation is symmetric. Running `--rollback` again returns to the version you
just left, with its own journal, so neither side's sessions are lost and a
mistaken rollback is itself reversible.

## Logs and health

With a LaunchAgent, output goes to `$SYLPH_WEB_PREFIX/logs/`. Each file is
rotated at startup once it passes 16 MB, keeping one previous generation.

```sh
tail -f ~/.local/share/sylph-web/logs/stderr.log
```

Two endpoints report state:

```sh
curl -s localhost:4317/api/v1/health        # liveness, no auth needed
curl -s localhost:4317/api/v1/diagnostics   # counts and uptime, needs a cookie
```

`/health` reports schema version and whether TraceDB is reachable. It stays
unauthenticated so a supervisor can probe it, and therefore reports no counts or
paths. `/diagnostics` adds session and turn counts, uptime, discovered agents,
and any runner that failed to describe itself.

Check the agent itself with:

```sh
launchctl list | grep dev.sylph.web
```

## Uninstalling

```sh
./scripts/install.sh --uninstall     # removes the install, keeps your journal
./scripts/install.sh --purge         # also deletes the journal
```

## Troubleshooting

**Port already in use.** The installer stops its own agent before checking, so
this means another process holds the port. Find it with
`lsof -ti :4317 -sTCP:LISTEN`, or install on a different port.

**No agents in the dropdown.** The backend found no runners. Check
`SYLPH_RUNNERS_DIR`, then `/api/v1/diagnostics` — a runner that exists but
failed to describe itself is listed there with its error rather than hidden.

**The archive is empty or search returns nothing.** Check `TRACEDB_BIN` points at
a real binary and that `/api/v1/health` reports `tracedb.available: true`.

**One half is up and the other is not.** It should not persist: the launcher
exits when either process dies, so the LaunchAgent restarts the pair. If it
recurs, `logs/stderr.log` has the reason.

**The agent will not stay loaded.** `launchctl list` shows a non-zero exit
status. The usual cause is a `PATH` without `node`; the installer writes the
`node` it built with into the plist, so a moved Node install needs a re-run.

