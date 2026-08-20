# Sylph Web

Local-first native UI for Sylph and coding-agent sessions archived by TraceDB.

Sylph Web can create Sylph-managed sessions, stream live tool/reasoning events, send local attachments, search stable cross-agent history, and resume sessions originally created in Claude Code, Codex, OpenCode, or Pi without changing their identity policy.

## Architecture

- **Backend:** Rust + Axum + SQLx + Tokio (Phase 1 in progress)
- **Frontend:** Next.js 16 + React 19 + TypeScript (Phase 2 starts week 4)
- **Database:** SQLite (live sessions) + TraceDB (stable history)
- **API:** REST + SSE streaming on `http://127.0.0.1:4317`

## Quick Start

This runs Sylph Web from the checkout, which is what you want while developing.
For a packaged install that starts at login and does not need the repo, see
[**docs/installation.md**](docs/installation.md).

### Prerequisites

- Rust 1.85+
- Node.js 22.5+
- TraceDB built from `~/Workspace/trace-db` (optional — without it, live
  sessions still work and archive search reports itself unavailable)
- At least one Sylph runner under `~/.agents/skills/sylph/runners`

### First Run

Two terminals, start to finish. Every command below has been run as written.

**Terminal 1 — backend:**

```sh
cd backend
cargo run
```

It binds `http://127.0.0.1:4317` and creates its database on first start.
Confirm it is up, and whether it found your archive:

```sh
curl -s http://127.0.0.1:4317/api/v1/health
# {"ok":true,"apiVersion":"1","schemaVersion":1,"tracedb":{"available":true}}
```

`tracedb.available: false` is not a failure. Live sessions work; only archive
search and native import are unavailable. Set `TRACEDB_BIN` to fix it.

**Terminal 2 — frontend:**

```sh
cd frontend
npm install
npm run dev
```

Open `http://localhost:3000`. The frontend proxies `/api/v1/*` to the backend
on the same origin, which is required: authentication is a `SameSite=Strict`
cookie that the browser would withhold across ports.

**Create a session and send one turn:**

1. Click **New session**.
2. Pick an agent from the dropdown. If it is empty, no runners were
   discovered — check `SYLPH_RUNNERS_DIR` and click refresh.
3. Set the working directory to a real path. The agent runs there.
4. Create, type a message, and send.

Events stream in as the agent works: thinking, tool calls, then the reply.
Closing the tab mid-turn is safe — reopening resumes the stream from where it
left off rather than replaying from the start.

**Import an archived session** (needs TraceDB): click **Search archive**, query for
something you remember saying, and import a hit. The session is restored with
its original agent identity and lifecycle. Sylph never injects `SOUL.md`,
resets, or rewrites prompts for an imported native session.

### Verifying a Checkout

```sh
cd backend && cargo test && cargo clippy
cd frontend && npm run build && npm run lint
cd frontend && npm run e2e
```

`npm run e2e` starts its own backend and frontend on separate ports with a
throwaway data directory and a mock runner, so it does not touch your journal,
your archive, or your real agents. It needs no servers running beforehand.

### Troubleshooting

- **Agent dropdown empty** — no runners discovered. Check
  `~/.agents/skills/sylph/runners` exists and its scripts are executable.
- **Browser 401s after a backend restart** — the auth token is regenerated on
  every start. Reload the page to pick up a fresh one.
- **Import fails with `TRACE_SESSION_NOT_RESUMABLE`** — the archive recorded no
  working directory for that session, so there is nowhere to resume it. Older
  `trace-db` versions did not record one.
- **Port 4317 in use** — set `SYLPH_WEB_PORT`, and point the frontend at it
  with `SYLPH_WEB_BACKEND`. For a production build, set `SYLPH_WEB_BACKEND`
  for `npm run build` as well: Next compiles the proxy target into the route
  manifest, so setting it only for `npm start` leaves the built app talking to
  the default port. `npm run dev` reads it on startup and needs no rebuild.

### Environment Variables

- `SYLPH_WEB_HOST` — bind address (default: `127.0.0.1`)
- `SYLPH_WEB_PORT` — port (default: `4317`)
- `SYLPH_WEB_DATA_DIR` — data directory (default: `~/.agents/data/sylph/web`)
- `SYLPH_RUNNERS_DIR` — runners directory (default: `~/.agents/skills/sylph/runners`)
- `TRACEDB_BIN` — TraceDB binary path (default: `~/Workspace/trace-db/target/release/trace-db`)

## Documentation

- [**Installation**](docs/installation.md) — Packaged install, upgrade, rollback
- [**AGENTS.md**](AGENTS.md) — Development guide and project rules
- [**Architecture**](docs/architecture.md) — System design and data flow
- [**Roadmap**](docs/roadmap.md) — Release phases and milestones
- [**API Design**](docs/api-design.md) — REST endpoints and SSE streaming
- [**Database Schema**](docs/database-schema.md) — SQLite schema and migrations
- [**Frontend Architecture**](docs/frontend-architecture.md) — UI design and implementation (Phase 2)

## Project Status

Sylph Web is usable end to end today: create a session, stream a turn, search
the archive, import a native session.

- ✅ **Phase 1** — backend core: sessions, turns, SSE, runner discovery, auth
- ✅ **Phase 2** — frontend foundation: session list, conversation view, resume
- ✅ **Phase 3** — attachments, archive search, native import, filters
- 🚧 **Phase 4** — production readiness: coverage, accessibility, performance
- ⬜ **Phase 5** — packaging and deployment

See [PHASE4_HANDOFF.md](PHASE4_HANDOFF.md) for what is deferred and why.

See [roadmap](docs/roadmap.md) for details.

## Contributing

Read [AGENTS.md](AGENTS.md) for development guidelines.

**Key rules:**
- Code, docs, commits: English only
- Conventional Commits with detailed messages
- Never `git add -A` — stage files explicitly
- Update specs before changing design
- Test before committing

## License

Private project
