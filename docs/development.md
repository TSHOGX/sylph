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
3. **TraceDB built** (optional):
   ```sh
   cd ~/Workspace/trace-db && cargo build --release
   ```

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

Everything else — the data directory, browse roots, the TraceDB binary — belongs in
`~/.sylph/config.toml` rather than in a shell export, so it survives a new terminal
and a supervisor restart. See [**configuration.md**](configuration.md).

## Quick Health Check

### Backend Status

Look for these lines in the backend tab output:

```
✓ Good:
  INFO sylph_web_backend: Sylph home /Users/you/.sylph (config /Users/you/.sylph/config.toml)
  INFO sylph_web_backend: Database initialized at /Users/you/.sylph/data/journal.db
  INFO sylph_web_backend::runners: Discovered agent claude at "..."
  INFO sylph_web_backend: Agent discovery: 2 usable, 0 broken
  INFO sylph_web_backend: TraceDB available: 4705 archived sessions, 529688 events
  INFO sylph_web_backend: Listening on http://127.0.0.1:4317

✗ Issues:
  - "TraceDB unavailable" → not on PATH; set tracedb.bin in config.toml
  - "address already in use" → Port 4317 is taken (change SYLPH_PORT)
  - "No usable agents" → logged at ERROR, and it names every directory searched
```

Zero usable agents is logged at `error`, not `info`: no session can be created and
no slash command offered, so the server reports healthy while unable to do its job.
`cargo run -- doctor` prints every resolved path and whether it exists.

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
  "tracedb": {
    "available": true
  }
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
4. **Test search:** Click search icon (requires TraceDB)

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

### The start screen lists few sessions, and the archive seems missing

**This is expected.** The start screen's recents come from the live journal
(`journal.db`), which holds only what this app created or imported. It lists
directories rather than sessions, so a directory worked in all week is one row.

Archived coding-agent history lives in TraceDB and is reached three ways:

1. **The sidebar** — selecting a directory shows its archived sessions alongside
   live ones, marked `⌗`, with `⑂N` for rolled-up subagents
2. **`Cmd+Shift+K`** — pages one directory's full archived history
3. **`Cmd+K`** — searches live sessions and the archive together by content

The architecture separates:
- **Active sessions** (journal.db) — ongoing work, resumable, editable
- **Archived sessions** (TraceDB) — historical read-only records, searchable

An archived session opens read-only and offers explicit continuation. Importing is
not required to read one; it is required to make it a live, resumable session, and
is disabled with its reason when the archive recorded no working directory.

If a directory that you know holds history looks empty, check
`/api/v1/health` for `tracedb.available` first.

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

### Backend not finding TraceDB

**Symptom:** Startup shows:
```
WARN sylph_web_backend: TraceDB unavailable, so archive search and native session
                        import will return no results.
```

**Fix:** TraceDB is a separate project. Put it on your `PATH`, or pin a checkout
build in `~/.sylph/config.toml`:
```toml
[tracedb]
bin = "~/Workspace/trace-db/target/release/trace-db"
```

Then confirm what resolved:
```sh
cargo run -- doctor        # the tracedb.bin row says found or MISSING
```

### "No agents discovered" / Agent dropdown empty

**Symptom:** Frontend agent selector is empty, or backend logs at `error`:
```
ERROR sylph_web_backend: No usable agents. Sessions cannot run and no slash
                         commands will be offered.
```

**Fix:** The log line names every directory searched, and so does `doctor`:
```sh
cargo run -- doctor
# "Bundled adapters" lists each candidate, with -> on the one in use.
# "Your agents" counts adapter.sh trees in ~/.sylph/agents.
```

Adapters ship with the repo in `adapters/`, so a checkout should find them with
nothing configured. An adapter that was found but failed its handshake appears in
`/api/v1/diagnostics` with its error rather than being hidden.

## Logs and Debugging

### Backend logs

Already visible in the herdr backend tab. For more detail:
```sh
RUST_LOG=debug cargo run
```

Levels: `error`, `warn`, `info` (default), `debug`, `trace`

### Frontend logs

Already visible in the herdr frontend tab. Next.js Turbopack shows:
- Request logs with timing
- Cross-origin blocking warnings
- Build/compilation errors

### Browser DevTools

**Network tab:**
- Check for failed API requests (`/api/v1/*`)
- Verify auth cookie is sent on requests
- Look for 403s on `/_next/*` chunks (cross-origin issue)

**Console tab:**
- React hydration errors
- WebSocket connection failures (HMR)

## Workflow Tips

### Restarting services

**Backend changes:**
- Code changes: Ctrl-C and re-run `cargo run` (or use `cargo watch`)
- Config changes: Just restart

**Frontend changes:**
- Code changes: Hot reload automatic (no restart needed)
- `next.config.ts` changes: **Must restart** `npm run dev`
- Environment variable changes: **Must restart**

### Testing across devices

1. Find your machine's LAN IP:
   ```sh
   ifconfig en0 | grep "inet " | awk '{print $2}'
   # e.g., 100.125.12.35
   ```

2. Add it to `SYLPH_WEB_DEV_ORIGINS`:
   ```sh
   export SYLPH_WEB_DEV_ORIGINS="100.125.12.35"
   npm run dev
   ```

3. Access from another device on the same network:
   ```
   http://100.125.12.35:3000
   ```

This direct LAN URL is read-only: mutations 403 because the backend rejects the
non-loopback `Origin`. For full read/write access from the LAN, run the loopback
proxy bound to `0.0.0.0` (see "Internet access via cpolar" above).

### Running tests

**Backend:**
```sh
cd backend
cargo test              # All tests
cargo test tracedb      # Just TraceDB module
cargo test --test integration import  # Import tests only
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

- **Backend** (Rust): Handles sessions, runners, database, TraceDB client
- **Frontend** (Next.js): UI, routing, SSE client, proxies API requests

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
- `~/Library/Application Support/trace-db/trace.db` — archived sessions (TraceDB, macOS)

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
rm ~/.sylph/data/journal.db   # Deletes active sessions; history.db is separate
# TraceDB archive is never touched
```
