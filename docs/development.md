# Development Deployment

Running Sylph Web from a checkout, including LAN and tunnelled access.

For packaged production installs, see [**installation.md**](installation.md). For the
simplest two-terminal start, see the Quick Start in [**README.md**](../README.md).

`$REPO` below is your checkout path.

## Overview

- **Backend** (Rust): `cargo run` on `127.0.0.1:4317`
- **Frontend** (Next.js): `npm run dev` on `localhost:3000`

Two processes, so run them in two panes of whatever terminal multiplexer or task runner
you use — the examples below use [herdr](https://github.com/TSHOGX/herdr) tabs, but nothing
here depends on it.

## Setup

### Prerequisites

1. **Project dependencies:**
   ```sh
   cd $REPO/crates/server && cargo build
   cd $REPO/frontend && npm install
   ```
2. **At least one agent CLI on your `PATH`** — `claude`, `codex`, or `pi`. That is the
   whole availability check; there is nothing else to install.

### Two panes

**Backend**
```sh
cd $REPO/crates/server
cargo run
```

**Frontend** — `SYLPH_WEB_DEV_ORIGINS` is only needed for LAN access, see below:
```sh
cd $REPO/frontend
export SYLPH_WEB_DEV_ORIGINS="<lan-ip>"
npm run dev
```

### Environment Variables

**Frontend tab:**
- `SYLPH_WEB_DEV_ORIGINS` — Comma-separated hostnames (no protocol/port) for
  cross-origin dev access. Needed only when the browser reaches the dev server
  directly from a non-loopback origin, i.e. LAN IP access. Omit for
  localhost-only development and for the cpolar setup below (its reverse proxy
  rewrites `Origin` to `localhost`).

  Examples:
  - LAN IP: `"<lan-ip>"`
  - Multiple LAN IPs: `"<lan-ip>,<other-lan-ip>"`

**Backend tab (optional overrides):**
- `SYLPH_HOME` — the root holding config, identity, data, and scratch
  (default: `~/.sylph`). One variable relocates the whole install, which is what
  makes a second instance a one-line change.
- `SYLPH_PORT` — Backend port (default: `4317`)
- `SYLPH_LOG` — Log level (default: `info`, try `debug` for troubleshooting)

Everything else — the data directory, the identity file, the Quick Start directory —
belongs in `~/.sylph/config.toml` rather than in a shell export, so it survives a new terminal
and a supervisor restart. See [**configuration.md**](configuration.md).

## Quick Health Check

### Backend Status

Look for these lines in the backend tab output:

```
✓ Good:
  INFO sylph_server: Sylph home /Users/you/.sylph (config /Users/you/.sylph/config.toml)
  INFO sylph_server: Database initialized at /Users/you/.sylph/data/journal.db
  INFO sylph_server: Listening on http://127.0.0.1:4317
  INFO sylph_server::env: Environment env7fk2 "dev" connected: claude 2.1.247, pi 0.84.3

✗ Issues:
  - "address already in use" → Port 4317 is taken (change SYLPH_PORT)
  - no environment line → the envagent is not running or not paired
```

**A server with no environment connected can hold history and start nothing**, so it
reports healthy while unable to do the thing you are about to try. That is why the
connection is logged at `info` on both sides and reported per environment on `/health`
rather than folded into one boolean. `cargo run -p sylph-server -- doctor` prints every
resolved path; `cargo run -p sylph-envagent -- doctor` prints which agent CLIs it found
and which server it is paired to.

Or check via curl:
```sh
curl -s http://127.0.0.1:4317/api/v1/health | jq
```

Expected output:
```json
{
  "ok": true,
  "apiVersion": "1",
  "schemaVersion": 1,
  "environments": [
    {"id": "env7fk2", "name": "dev", "connected": true,
     "agents": [{"name": "claude", "version": "2.1.247"}]}
  ]
}
```

### Frontend Status

Look for these lines in the frontend tab output:

```
✓ Good:
  ▲ Next.js 16.3.1 (Turbopack)
  - Local:         http://localhost:3000
  - Network:       http://<lan-ip>:3000
  ✓ Ready in 290ms

✗ Issues:
  - "⚠ Blocked cross-origin request to Next.js dev resource /_next/hmr from '...'"
    → Add that hostname to SYLPH_WEB_DEV_ORIGINS and restart
  - "⚠ Invalid next.config.ts options detected"
    → Config syntax error, check frontend/next.config.ts
  - "Port 3000 is already in use"
    → Change port with: PORT=3001 npm run dev
```

### Full Stack Test

1. **Open in browser:** http://localhost:3000
2. **Verify UI loads:** the start screen, with the workspaces sidebar beside it.
   On a fresh install it shows the first-run guide; once sessions exist it lists
   the most recent instead
3. **Check auth:** Open DevTools Network tab, refresh page, verify no 401s on `/api/v1/*` requests
4. **Test search:** Click search icon

## Access Methods

| URL | Access From | Requires |
|-----|-------------|----------|
| http://localhost:3000 | Same machine | Nothing (default) |
| http://<lan-ip>:3000 | LAN devices | `SYLPH_WEB_DEV_ORIGINS="<lan-ip>"` |
| https://<id>.r8.cpolar.cn | Internet (via cpolar) | loopback reverse proxy + cpolar (below) |

### Internet access via cpolar

Tunnelling straight to `localhost:3000` is not enough: the backend rejects
non-loopback `Host`/`Origin` headers (DNS-rebinding defense, see
`crates/server/src/middleware/auth.rs`), and Next's rewrite forwards the browser's
`Origin`, so every mutation (create/import/resume) 403s from a tunnel or LAN
origin. Route through the loopback reverse proxy instead, which rewrites
`Host`/`Origin` to loopback and sends `/api/*` straight to the backend:

```sh
python3 $CPOLAR_DIR/loopback-proxy.py 47317 3000 4317
$CPOLAR_DIR/cpolar http -httpauth="user:password" -region=cn 47317
```

- `-region=cn` is load-bearing. The default r17 international line runs about
  110 KB/s, and the dev bundle (~3.5 MB of JS chunks) never finishes loading,
  so the page stays frozen on "Connecting to backend...". The r8 line is ~5×
  faster.
- `-httpauth` is the second gate in front of the loopback proxy; never expose
  the proxy without one.
- The public hostname is random per restart and appears in cpolar's output or
  the web UI at `http://127.0.0.1:4040`.
- No `SYLPH_WEB_DEV_ORIGINS` entry is needed for the tunnel: the proxy rewrites
  `Origin` to `localhost`, which Next already trusts.

For LAN, the same proxy can bind `0.0.0.0` instead (`PROXY_BIND=0.0.0.0
PROXY_AUTH=user:password`) so phones on the same network also get full
read/write access rather than the read-only direct frontend.

## Common Issues

### The start screen lists few sessions

**This is expected, and it is about grouping rather than about coverage.** The start
screen's recents list *directories*, not sessions, so a directory worked in all week is
one row.

History is one store the server owns, holding both what Sylph ran and what an adapter read
out of an agent's native files, so a session reaches every read surface the same way:

1. **The sidebar** — selecting a directory shows its sessions, with delegated work folded
   into its parent as a count
2. **`Cmd+Shift+K`** — pages one directory's full history
3. **`Cmd+K`** — full-text search over everything the server holds

**What a session can *do* is a separate question from where it is stored, and conflating
the two is the mistake the old two-class model encoded.** Every session is one class
(`docs/architecture.md` § One class of session). What varies is the **link's two facets**:
whether Sylph can *observe* it and whether Sylph can *drive* it. A session running in a
terminal right now is fully readable and not drivable — the process is alive and belongs
to whoever started it — and `pi` never offers a second party a control channel at all
(`docs/agents.md`).

And `fidelity` says how much resolution a record has: `stream` when Sylph held the
process's stdio, `blocks` or `messages` when it came from a vendor's native store. Not
correlated with who started the session — a session Sylph launched, read back after its
process died, is `messages` too.

If a directory you know holds history looks empty, check `/api/v1/health` for whether the
environment that ran it is connected.

### Cross-origin 403 errors on `/_next/*` chunks

**Symptom:** Browser console shows:
```
GET http://<lan-ip>:3000/_next/static/chunks/lib_1zew020._.js net::ERR_ABORTED 403
⚠ Blocked cross-origin request to Next.js dev resource /_next/hmr from "<lan-ip>"
```

**Fix:**
1. Add the hostname to `SYLPH_WEB_DEV_ORIGINS` (no protocol, no port)
2. Restart the frontend dev server

Example:
```sh
export SYLPH_WEB_DEV_ORIGINS="<lan-ip>"
npm run dev
```

### No environment connected

**Symptom:** `/api/v1/health` reports no connected paired environment, and a remote
session cannot be created.

**Fix:** start the envagent and pair it. In development:

```sh
# Terminal 1: HTTP on 4317, envagent ingress on 4318
cargo run -p sylph-server
# Terminal 2: obtain a code from POST /api/v1/environments/pairings, then pair
cargo run -p sylph-envagent -- pair --server http://127.0.0.1:4317 --code <code>
cargo run -p sylph-envagent -- run
```

Ingress binds one port above HTTP, and `pair` records the endpoint, so `run` needs no
address (`docs/configuration.md` § Ports).

Get the code from `POST /api/v1/environments/pairings`. The credential is written to
`~/.sylph-env/`, so pairing is once per machine, not once per run.

**The server does not embed an envagent**, so "everything on one laptop" is still two
processes (`docs/environments.md`).

### An agent is missing from an environment

**Symptom:** the agent selector offers fewer agents than you expected, or none.

**Fix:** an agent is available on an environment when its CLI is on **that machine's**
`PATH`. There is no adapter directory, no discovery pass, and no handshake to fail.

```sh
cargo run -p sylph-envagent -- doctor
# lists each agent CLI it found, with the version the CLI reported,
# and names the ones it looked for and did not find
```

**Two questions that used to produce one empty list now answer separately**, because they
have different fixes: `/api/v1/health` says whether the *environment* is connected, and
the envagent's own report says which *CLIs* it found there.

## Running the checks

Run the checks for every part of the repo a change touched. The two sides share a wire
contract, so an edit on either can break the other's tests.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo doc --workspace --no-deps
```

```sh
cd frontend
npm run build           # production build, and the TypeScript gate
npx eslint .            # lint
npm test                # Vitest
npm run e2e             # end-to-end, spawning its own servers
```

Narrower runs while iterating: `cargo test -p sylph-adapters` for vendor shape and
native-store parsing, `cargo test --test integration adopt` for adopting a native session.
`docs/frontend-architecture.md` § E2E owns how the browser suite is written and run.

## Architecture Notes

### Why two separate processes?

- **Server** (Rust): sessions, turns, links, policy, storage, search
- **Envagent** (Rust): runs the agent CLIs, holds the adapters, reads native stores,
  dials back to the server
- **Frontend** (Next.js): UI, routing, SSE client, proxies API requests

Three processes in development, not two, and the envagent's separateness is deliberate
(`docs/environments.md`).

The frontend's rewrite in `next.config.ts` makes the browser see one origin,
which is required for `SameSite=Strict` auth cookies to work.

### Port layout

`4317` HTTP and SSE, `4318` envagent ingress, `3000` frontend, all on `127.0.0.1`.
`docs/configuration.md` § Ports owns the allocation and the offset rule.

For remote access:
- Route through the loopback reverse proxy + cpolar (see "Internet access via
  cpolar" above) — the only path with full read/write access, because the
  backend rejects non-loopback `Origin` on mutations
- Or bind Next.js to `0.0.0.0` for read-only LAN access (not recommended for
  security)
- Backend stays on loopback

### Data locations

Everything Sylph owns is under `~/.sylph` — one root, so a backup or a reset has one
target (`docs/configuration.md`):

- `config.toml`, `SOUL.md`, `scratch/`
- `data/journal.db` — sessions, turns, and events still in flight
- `data/history.db`, `data/blobs/` — consolidated history and the full-text index, derived
  from the journal rather than separately configurable

An agent's own native files stay where that vendor puts them and are never written to.

## Cleanup

To stop services:
1. Press Ctrl-C in each pane
2. Or kill processes:
   ```sh
   pkill -f sylph-server
   pkill -f "next dev"
   ```

Data is never deleted by stopping services. To reset:
```sh
rm -rf ~/.sylph/data          # journal, history, blobs, attachments — one subtree
# An agent's own native files are never touched, which is what makes this safe:
# a session can be adopted again from the vendor's store.
```
