# Sylph Web

Local-first native UI for Sylph and coding-agent sessions archived by TraceDB.

Sylph Web can create Sylph-managed sessions, stream live tool/reasoning events, send local attachments, search stable cross-agent history, and resume sessions originally created in Claude Code, Codex, OpenCode, or Pi without changing their identity policy.

## Architecture

- **Backend:** Rust + Axum + SQLx + Tokio
- **Frontend:** Next.js 16 + React 19 + TypeScript + TanStack Query
- **Database:** SQLite (live sessions) + TraceDB (stable history)
- **API:** REST + SSE streaming on `http://127.0.0.1:4317`

## Quick Start

This runs Sylph Web from the checkout, which is what you want while developing.

- **For herdr-based deployment** (recommended for development with remote access):
  see [**docs/development.md**](docs/development.md)
- **For packaged production installs**: see [**docs/installation.md**](docs/installation.md)

The two-terminal setup below is the simplest way to start, but doesn't cover
cpolar tunnels or LAN access — use the development guide for those scenarios.

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

The start screen walks a first run itself. In short:

**Create a session and send one turn:**

1. Click **New session** (or press `Cmd+N` / `Ctrl+N`) and choose a directory. That
   is the only thing a session needs — the agent runs there and can read and write
   nothing outside it.
2. Type a message and send. The agent is switchable until that first message; model
   and reasoning effort are per-turn controls in the composer.

If the agent list is empty, no runners were discovered — check
`SYLPH_RUNNERS_DIR`.

Events stream in as the agent works: thinking, tool calls, then the reply.
Closing the tab mid-turn is safe — reopening resumes the stream from where it
left off rather than replaying from the start.

**Read what agents already did here** (needs TraceDB): press `Cmd+Shift+K` to page
a directory's archived history, or `Cmd+K` to search every session and the archive
together. Archived transcripts open read-only.

**Import an archived session** (needs TraceDB): from **Search archive**, query for
something you remember saying, and import a hit. The session is restored with
its original agent identity and lifecycle. Sylph never injects `SOUL.md`,
resets, or rewrites prompts for an imported native session.

### Verifying a Checkout

```sh
cd backend && cargo test && cargo clippy --all-targets
cd frontend && npm test && npm run build && npx eslint .
cd frontend && npm run e2e
```

That is 132 backend unit + 116 backend integration + 193 frontend unit + 49 E2E.
`npm run build` type-checks, so it is the TypeScript gate — there is no separate
`tsc` step. `cargo` is not on the default `PATH`; use `~/.cargo/bin/cargo` or export
it first.

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

Comprehensive documentation lives in `docs/`:

- [**Documentation Index**](docs/README.md) — Start here for navigation and principles
- [**Development**](docs/development.md) — herdr deployment, health checks, troubleshooting
- [**Installation**](docs/installation.md) — Packaged install, upgrade, rollback
- [**Architecture**](docs/architecture.md) — System design and data flow
- [**Frontend Architecture**](docs/frontend-architecture.md) — UI design and implementation
- [**Roadmap**](docs/roadmap.md) — Project status and future enhancements
- [**API Design**](docs/api-design.md) — REST endpoints and SSE streaming
- [**Database Schema**](docs/database-schema.md) — SQLite schema and migrations
- [**Agent UI Design Analysis**](docs/agent-ui-design-analysis.md) — Why agent workspaces differ from chat UIs
- [**Keyboard Shortcuts**](docs/keyboard-shortcuts-default-sessions-settings.md) — Shortcuts, Quick Start, Settings

The last two are pre-implementation design arguments, kept for *why*. Where they
differ from what shipped, `frontend-architecture.md` and `roadmap.md` win.

See [**AGENTS.md**](AGENTS.md) for the project's development rules and conventions.

## What It Does

**Runs sessions.** A session is a directory plus an agent. Creating one means naming
a directory — from the sidebar, the start screen, or a picker — not filling a form.
The composer exposes the model and reasoning effort each runner declares for itself,
so `codex` shows effort with no model selector and nothing is hardcoded.

**Streams work as it happens.** Turns are server-side resources, not request
lifetimes: closing the tab mid-turn and reopening resumes the stream from where it
left off rather than replaying it, and a backend restart marks orphaned turns
`interrupted` instead of leaving them running forever.

**Shows tool execution, not chat.** The timeline is flat and anchored by tool calls.
An edit renders its diff inline; a failed command shows its output without being
asked; a successful read stays closed. Clicking any file path opens it in the
Artifacts pane, which previews Excel, Word, PowerPoint, CSV, images, HTML, and PDF
alongside code and Markdown — parsed server-side, so no document parser runs in the
browser.

**Reads history that predates it.** The sidebar groups sessions by the directory they
ran in and merges live rows with TraceDB's archive, so a directory shows what agents
did there before this app existed. `Cmd+Shift+K` pages one directory's history;
`Cmd+K` searches live sessions and the archive together. An archived session opens
read-only and can be continued explicitly.

**Keeps native sessions native.** A session imported from another agent preserves
that agent's identity and lifecycle. Sylph never injects `SOUL.md`, resets, or
rewrites prompts for one — the backend sends the empty `SYLPH_SOUL` that the runner
protocol reserves for exactly that.

Deliberately absent, with reasons in [roadmap](docs/roadmap.md): dark theme, session
delete/move/export, "Open in Default App", and any non-loopback access story.

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
