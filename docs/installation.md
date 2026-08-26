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

Sylph keeps everything it owns under `~/.sylph` and takes its settings from
`~/.sylph/config.toml`. Both are created on first start. See
[**configuration.md**](configuration.md) for the file and the full settings table.

The installer itself reads only these:

| Variable | Default | What it controls |
| --- | --- | --- |
| `SYLPH_HOME` | `~/.sylph` | Config, identity, data, scratch — everything Sylph owns |
| `SYLPH_PORT` | `4317` | Backend port |
| `SYLPH_WEB_FRONTEND_PORT` | `3000` | Frontend port |
| `SYLPH_WEB_PREFIX` | `~/.local/share/sylph-web` | Where the install lives |
| `SYLPH_WEB_BIN_DIR` | `~/.local/bin` | Where launchers are linked |

Set them for the install command, not just at start time:

```sh
SYLPH_PORT=4400 SYLPH_WEB_FRONTEND_PORT=3400 ./scripts/install.sh
```

The backend port in particular has to be set at build time. Next compiles its
proxy target into the route manifest, so a port supplied only when starting is
ignored and the UI talks to `4317` regardless. The installer handles this; it is
worth knowing if you build by hand.

Everything else — the data directory, your own agents, the identity preamble, the
TraceDB binary, browse roots, log level — is set in `config.toml` rather than
here, so it is not something the installer has to know about or propagate. That is
the point of the single root: the LaunchAgent plist carries `SYLPH_HOME` and does
not grow a key every time a setting is added.

## Your data

Everything is under `~/.sylph`:

```
~/.sylph/
├── config.toml      Your settings. Edit freely; commented on creation.
├── SOUL.md          The identity preamble. Yours to edit or delete.
├── agents/          Agent adapters you added.
├── data/            The journal, history, blobs, attachments. Back this up.
└── scratch/         Where Quick Start sessions run.
```

`data/` is the one directory that matters for backup, and it is one subtree on
purpose: the two SQLite tiers, the blob store, and staged attachments reference
each other, so copying `data/` copies a consistent whole and copying part of it
does not.

The installer touches only `data/`, and it survives upgrades and uninstalls.
`--purge` deletes the whole root, which is the one operation that needs asking
for.

To check what actually resolved:

```sh
sylph-web-backend doctor
```

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

## Moving data from an earlier install

Sylph used to keep its journal under `~/.agents/data/sylph/web`. Nothing reads
that location any more — a path kept as a fallback is a second layout nobody
maintains — so an existing install brings its data across explicitly, once:

```sh
sylph-web-backend migrate --from ~/.agents/data/sylph/web
```

That copies the journal, consolidated history, blob store, and staged attachments
into `~/.sylph/data`, leaving the original untouched. It refuses rather than
merging if `~/.sylph/data` already holds a journal: two journals cannot be
combined, since session ids are unique per store and the blob index would
reference rows that are not there. Move the existing one aside first, or pass
`--force` to replace it.

Stop the server before migrating, so nothing is mid-write.

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
and any adapter that failed its handshake.

Check the agent itself with:

```sh
launchctl list | grep dev.sylph.web
```

## Uninstalling

```sh
./scripts/install.sh --uninstall     # removes the install, keeps ~/.sylph
./scripts/install.sh --purge         # also deletes ~/.sylph
```

`--purge` removes your config, identity file, sessions, and history. It is a
separate flag for that reason.

## Troubleshooting

Start with `sylph-web-backend doctor`. It prints every resolved path, which source
supplied it, and whether it exists — which is the answer to most of what follows.
Settings → Configuration shows the same thing in the UI.

**Port already in use.** The installer stops its own agent before checking, so
this means another process holds the port. Find it with
`lsof -ti :4317 -sTCP:LISTEN`, or install on a different port.

**No agents in the dropdown.** The backend discovered none. `doctor` reports how
many adapters it found and every directory it searched; `/api/v1/diagnostics` adds
any that were found but failed their handshake, with the error rather than hidden.

**The archive is empty or search returns nothing.** TraceDB is a separate install.
Check `doctor` marks `tracedb.bin` as found, and that `/api/v1/health` reports
`tracedb.available: true`. Set `tracedb.bin` in `config.toml` if it lives in a
checkout rather than on `PATH`.

**A setting in `config.toml` seems to be ignored.** An environment variable
overrides the file. `doctor` names the source of every value, so a row reading
`SYLPH_…` rather than `config.toml` is the answer.

**One half is up and the other is not.** It should not persist: the launcher
exits when either process dies, so the LaunchAgent restarts the pair. If it
recurs, `logs/stderr.log` has the reason.

**The agent will not stay loaded.** `launchctl list` shows a non-zero exit
status. The usual cause is a `PATH` without `node`; the installer writes the
`node` it built with into the plist, so a moved Node install needs a re-run.

