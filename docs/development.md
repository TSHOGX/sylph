# Development Deployment

This guide covers running Sylph Web in development mode using herdr for local
testing and remote access scenarios.

For packaged production installs, see [**installation.md**](installation.md).
For a quick two-terminal setup without herdr, see the Quick Start in
[**README.md**](../README.md).

## Overview

Development deployment runs:
- **Backend** (Rust): `cargo run` on `127.0.0.1:4317`
- **Frontend** (Next.js): `npm run dev` on `localhost:3000`

Both run in separate herdr tabs for easy monitoring and log access.

## Setup in herdr

### Prerequisites

1. **herdr installed and configured**
2. **Project dependencies:**
   ```sh
   cd /Users/hsw/Workspace/sylph-web/backend && cargo build
   cd /Users/hsw/Workspace/sylph-web/frontend && npm install
   ```
3. **At least one agent CLI on your `PATH`** — `claude`, `codex`, or `pi`. That is
   what makes an agent available to an envagent; there is nothing else to install.

### Create herdr tabs

In your herdr configuration, create two tabs in the `sylph-web` workspace:

**Tab 1: `backend`**
```sh
export PATH="$HOME/.cargo/bin:$PATH"
cd /Users/hsw/Workspace/sylph-web/backend
cargo run
```

**Tab 2: `frontend`**
```sh
cd /Users/hsw/Workspace/sylph-web/frontend
export SYLPH_WEB_DEV_ORIGINS="100.125.12.35"
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
  - LAN IP: `"100.125.12.35"`
  - Multiple LAN IPs: `"100.125.12.35,192.168.1.100"`

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
  INFO sylph_web_backend: Sylph home /Users/you/.sylph (config /Users/you/.sylph/config.toml)
  INFO sylph_web_backend: Database initialized at /Users/you/.sylph/data/journal.db
  INFO sylph_server: Listening on http://127.0.0.1:4317
  INFO sylph_server::env: Environment env_7fk2 "dev" connected: claude 2.1.247, pi 0.84.3

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
  "schemaVersion": 7,
  "environments": [
    {"id": "env_7fk2", "name": "dev", "connected": true,
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
  - Network:       http://100.125.12.35:3000
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
| http://100.125.12.35:3000 | LAN devices | `SYLPH_WEB_DEV_ORIGINS="100.125.12.35"` |
| https://<id>.r8.cpolar.cn | Internet (via cpolar) | loopback reverse proxy + cpolar (below) |

### Internet access via cpolar

Tunnelling straight to `localhost:3000` is not enough: the backend rejects
non-loopback `Host`/`Origin` headers (DNS-rebinding defense, see
`backend/src/middleware/auth.rs`), and Next's rewrite forwards the browser's
`Origin`, so every mutation (create/import/resume) 403s from a tunnel or LAN
origin. Route through the loopback reverse proxy instead, which rewrites
`Host`/`Origin` to loopback and sends `/api/*` straight to the backend:

```sh
python3 ~/.agents/data/cpolar/loopback-proxy.py 47317 3000 4317
~/.agents/data/cpolar/cpolar http -httpauth="user:password" -region=cn 47317
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

There is no separate archive to look in. History is one store the server owns, holding
both what Sylph ran and what an adapter read out of an agent's native files, so a
session reaches every read surface the same way:

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
GET http://100.125.12.35:3000/_next/static/chunks/lib_1zew020._.js net::ERR_ABORTED 403
⚠ Blocked cross-origin request to Next.js dev resource /_next/hmr from "100.125.12.35"
```

**Fix:**
1. Add the hostname to `SYLPH_WEB_DEV_ORIGINS` (no protocol, no port)
2. Restart the frontend dev server

Example:
```sh
export SYLPH_WEB_DEV_ORIGINS="100.125.12.35"
npm run dev
```

### No environment connected

**Symptom:** `/api/v1/health` reports an empty `environments` array, and no session can
be created.

**Fix:** start the envagent and pair it. In development:

```sh
cargo run -p sylph-envagent -- pair --server http://127.0.0.1:4317 --code <code>
cargo run -p sylph-envagent
```

Get the code from `POST /api/v1/environments/pairings`. The credential is written to
`~/.sylph-env/`, so pairing is once per machine, not once per run.

**The server does not embed an envagent**, deliberately (`docs/environments.md`), so
"everything on one laptop" is still two processes. That is what keeps the server's own
machine from being the one environment configured differently from every other.

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
the envagent's own report says which *CLIs* it found there. Collapsing them is what made
a stale adapter tree present as a composer ignoring the `/` key — a symptom three layers
from the cause.

**Backend:**
```sh
cd backend
cargo test --workspace                  # All crates
cargo test -p sylph-adapters            # Vendor shape and native-store parsing
cargo test --test integration adopt     # Adopting a native session
```

**Frontend:**
```sh
cd frontend
npm run build           # Verify production build works
npm run lint            # TypeScript and ESLint
npm run e2e             # End-to-end tests (spawns own servers)
```

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

- `4317` — Backend API and SSE
- `3000` — Frontend (Next.js dev server)
- Both default to `127.0.0.1` (loopback only)

For remote access:
- Route through the loopback reverse proxy + cpolar (see "Internet access via
  cpolar" above) — the only path with full read/write access, because the
  backend rejects non-loopback `Origin` on mutations
- Or bind Next.js to `0.0.0.0` for read-only LAN access (not recommended for
  security)
- Backend stays on loopback

### Data locations

- `~/.sylph/` — everything Sylph owns: `config.toml`, `SOUL.md`, `agents/`,
  `data/`, `scratch/`
- `~/.sylph/data/journal.db` — active sessions (SQLite)
- `~/.sylph/data/history.db`, `~/.sylph/data/blobs/` — consolidated history, derived from
  the journal rather than separately configurable
- `~/Library/Application Support/trace-db/trace.db` — archived sessions, **currently**,
  because the archive is still `trace-db`'s own process at `trace-db`'s own default. The
  design moves it under `~/.sylph` as a private store Sylph owns; the point of that is
  that one root holds everything Sylph has, so a reset or a backup has one target rather
  than two on different platform conventions. See `docs/architecture.md`.

## Cleanup

To stop services:
1. In herdr, switch to each tab and press Ctrl-C
2. Or kill processes:
   ```sh
   pkill -f sylph-web-backend
   pkill -f "next dev"
   ```

Data is never deleted by stopping services. To reset:
```sh
rm -rf ~/.sylph/data          # journal, history, blobs, attachments — one subtree
# An agent's own native files are never touched, which is what makes this safe:
# a session can be adopted again from the vendor's store.
```
