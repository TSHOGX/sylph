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
export SYLPH_WEB_DEV_ORIGINS="44fb4bb.r8.cpolar.cn,100.125.12.35"
npm run dev
```

### Environment Variables

**Frontend tab:**
- `SYLPH_WEB_DEV_ORIGINS` — Comma-separated hostnames (no protocol/port) for
  cross-origin dev access. Needed only for cpolar tunnels or LAN IP access.
  Omit for localhost-only development.
  
  Examples:
  - cpolar tunnel: `"44fb4bb.r8.cpolar.cn"`
  - LAN IP: `"100.125.12.35"`
  - Multiple: `"44fb4bb.r8.cpolar.cn,100.125.12.35,192.168.1.100"`

**Backend tab (optional overrides):**
- `SYLPH_WEB_PORT` — Backend port (default: `4317`)
- `SYLPH_WEB_DATA_DIR` — Database location (default: `~/.agents/data/sylph/web`)
- `TRACEDB_BIN` — TraceDB binary path (default: `~/Workspace/trace-db/target/release/trace-db`)
- `RUST_LOG` — Log level (default: `info`, try `debug` for troubleshooting)

## Quick Health Check

### Backend Status

Look for these lines in the backend tab output:

```
✓ Good:
  INFO sylph_web_backend: Starting Sylph Web backend on 127.0.0.1:4317
  INFO sylph_web_backend: Database initialized at /Users/hsw/.agents/data/sylph/web/live.db
  INFO sylph_web_backend::runners: Discovered runner: claude at "..."
  INFO sylph_web_backend: Runner discovery: 4 agents found, 0 broken
  INFO sylph_web_backend: TraceDB available: 4705 archived sessions, 529688 events
  INFO sylph_web_backend: Listening on http://127.0.0.1:4317

✗ Issues:
  - "TraceDB available: 0 archived sessions" → TraceDB is empty or pointing to wrong database
  - "address already in use" → Port 4317 is taken (change SYLPH_WEB_PORT)
  - "0 agents found" → No runners in ~/.agents/skills/sylph/runners
```

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
2. **Verify UI loads:** Should see "Sylph Web" header and session list
3. **Check auth:** Open DevTools Network tab, refresh page, verify no 401s on `/api/v1/*` requests
4. **Test search:** Click search icon (requires TraceDB)

## Access Methods

| URL | Access From | Requires |
|-----|-------------|----------|
| http://localhost:3000 | Same machine | Nothing (default) |
| http://100.125.12.35:3000 | LAN devices | `SYLPH_WEB_DEV_ORIGINS="100.125.12.35"` |
| https://xyz.r8.cpolar.cn | Internet (via cpolar) | `SYLPH_WEB_DEV_ORIGINS="xyz.r8.cpolar.cn"` |

**Note:** The cpolar hostname changes when the tunnel rotates. Update
`SYLPH_WEB_DEV_ORIGINS` each time and restart the frontend.

## Common Issues

### Only seeing one session on the home page

**This is expected.** The home page shows "active sessions" from the live journal
(`live.db`). Your archived sessions (stored in TraceDB) are accessed via:

1. **Search tab** — Click the search icon in the sidebar
2. **Import** — Search for a session, then import it to make it active

The architecture separates:
- **Active sessions** (live.db) — ongoing work, resumable, editable
- **Archived sessions** (TraceDB) — historical read-only records, searchable

To see your 4,000+ archived sessions, use the search function.

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
export SYLPH_WEB_DEV_ORIGINS="100.125.12.35,44fb4bb.r8.cpolar.cn"
npm run dev
```

### Backend not finding TraceDB

**Symptom:** Startup shows:
```
INFO sylph_web_backend: TraceDB available: 0 archived sessions, 0 events
```

**Fix:** Set the correct path:
```sh
export TRACEDB_BIN="/Users/hsw/Workspace/trace-db/target/release/trace-db"
cargo run
```

Or verify the default path exists:
```sh
ls -lh ~/Workspace/trace-db/target/release/trace-db
```

### "No runners discovered" / Agent dropdown empty

**Symptom:** Frontend agent selector is empty, or backend logs:
```
INFO sylph_web_backend: Runner discovery: 0 agents found, 0 broken
```

**Fix:** Ensure runners exist and are executable:
```sh
ls -l ~/.agents/skills/sylph/runners/*/runner.sh
# Should show executable scripts for claude, codex, pi, opencode, etc.
```

If missing, set up your Sylph runners first.

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
- Use cpolar or similar tunnel to `localhost:3000`
- Or bind Next.js to `0.0.0.0` (not recommended for security)
- Backend stays on loopback; frontend proxies to it

### Data locations

- `~/.agents/data/sylph/web/live.db` — Active sessions (SQLite)
- `~/Library/Application Support/trace-db/trace.db` — Archived sessions (TraceDB, macOS)
- `~/.agents/skills/sylph/runners/` — Runner discovery

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
rm ~/.agents/data/sylph/web/live.db  # Deletes active sessions
# TraceDB archive is never touched
```
