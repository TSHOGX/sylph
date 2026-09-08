# Sylph

Local-first workspace for coding-agent sessions: `claude`, `codex`, and `pi`.

Sylph creates and runs sessions, streams tool and reasoning events as they happen,
sends local attachments, searches every session it holds, and adopts sessions a vendor CLI
created on this machine without changing their identity.

The three agents are linked into the backend, so a checkout has agents with nothing to
install beyond the vendor CLIs themselves. An agent is available when its CLI is on your
`PATH`.

## Architecture

- **Backend:** Rust + Axum + SQLx + Tokio
- **Frontend:** Next.js 16 + React 19 + TypeScript + TanStack Query
- **Store:** SQLite — `journal.db` for turns in flight, `history.db` for consolidated
  blocks and the full-text index, `blobs/` beside them. One authoritative record, split
  for SQLite's sake.
- **API:** REST + SSE on `http://127.0.0.1:4317`

## Quick Start

This runs Sylph from the checkout, which is what you want while developing.

- **For herdr-based deployment** (recommended for development with remote access):
  see [**docs/development.md**](docs/development.md)
- **For packaged production installs**: see [**docs/installation.md**](docs/installation.md)

The two-terminal setup below is the simplest way to start, but doesn't cover
cpolar tunnels or LAN access — use the development guide for those scenarios.

### Prerequisites

- Rust 1.88+
- Node.js 22.5+
- At least one agent CLI on your `PATH`: `claude`, `codex`, or `pi`. Sylph reads each
  vendor's own session store where it already lives, so there is nothing to import and
  nothing to configure.

On first start Sylph creates `~/.sylph` with a fully commented `config.toml` and a
`SOUL.md` you can edit or delete.

### First Run

Two terminals, start to finish. Every command below has been run as written.

**Terminal 1 — backend:**

```sh
cd backend
cargo run
```

It binds `http://127.0.0.1:4317` and creates its store on first start.
Confirm it is up:

```sh
curl -s http://127.0.0.1:4317/api/v1/health
# {"ok":true,"apiVersion":"1","schemaVersion":1,"agents":{"available":true}}
```

`agents.available: false` *is* a failure, and the one worth checking first: with no
agent CLI found, every other probe answers successfully while no session can be created
and the composer's slash menu cannot open. `cargo run -- doctor` lists which agents were
found, and `GET /api/v1/environments` reports availability per machine — which is the
better question once more than one machine is paired, since "codex is not installed on
this laptop" is a fact about one environment.

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

1. Click **New session** (or press `Cmd+Shift+O` / `Ctrl+Shift+O`) and choose a
   directory. That is the only thing a session needs — the agent runs there and can
   read and write nothing outside it. Not `Cmd+N`: a browser answers that itself,
   above the page's handlers and beyond `preventDefault`
   (`docs/ui-layout.md` § Keyboard model).
2. Type a message and send. The agent is switchable until that first message; model
   and reasoning effort are per-turn controls in the composer, populated from what the
   live agent declares for itself.

Events stream in as the agent works: thinking, tool calls, then the reply.
Closing the tab mid-turn is safe — reopening resumes the stream from where it
left off rather than replaying from the start.

**Read what agents already did here:** press `Cmd+Shift+K` to page a directory's
history, or `Cmd+K` to search every session Sylph holds.

**Adopt an existing session:** `Cmd+P` → **Adopt an existing session** lists what the
vendor CLIs on this machine have recorded, filterable by agent and path. Adopting one
parses its native store into the same events a live turn emits, so it reads like any
other session and can be continued. Sylph never injects `SOUL.md` into a session it did
not create.

### Verifying a Checkout

```sh
cd backend && cargo test && cargo clippy --all-targets
cd frontend && npm test && npm run build && npx eslint .
cd frontend && npm run e2e
```

The suites report their own totals; treat them as a sanity check rather than a target.
`npm run build` type-checks, so it is the TypeScript gate — there is no separate
`tsc` step. `cargo` is not on the default `PATH`; use `~/.cargo/bin/cargo` or export
it first.

`npm run e2e` starts its own backend and frontend on separate ports with a
throwaway `SYLPH_HOME` and a fixture agent, so it does not touch your store or your
real agents. It needs no servers running beforehand, and refuses to run against a
server left behind by an interrupted one — clear its ports if it complains:

```sh
for p in 4327 4328 3101 3199; do lsof -ti tcp:$p | xargs kill 2>/dev/null; done
```

### Troubleshooting

- **Agent dropdown empty** — no agent CLI was found on `PATH`. The startup log says so
  at `error` level and names what it looked for; `cargo run -- doctor` lists the agents
  it found, and `/api/v1/environments` reports availability per machine with a reason.
- **Browser 401s after a backend restart** — the auth token is regenerated on
  every start. Reload the page to pick up a fresh one.
- **Adopting fails with `NATIVE_SESSION_NOT_RESUMABLE`** — the vendor recorded no
  working directory for that session, so there is nowhere to run it. Its transcript
  still reads.
- **Port 4317 in use** — set `SYLPH_PORT`, and point the frontend at it
  with `SYLPH_WEB_BACKEND`. For a production build, set `SYLPH_WEB_BACKEND`
  for `npm run build` as well: Next compiles the proxy target into the route
  manifest, so setting it only for `npm start` leaves the built app talking to
  the default port. `npm run dev` reads it on startup and needs no rebuild.

### Configuration

Everything Sylph owns lives under `~/.sylph`, and everything you configure is in
`~/.sylph/config.toml` — created, fully commented, on first start:

```
~/.sylph/
├── config.toml      Your settings.
├── SOUL.md          The identity preamble. Yours to edit or delete.
├── data/            Journal, history, blobs, attachments. Back this up.
└── scratch/         Where Quick Start sessions run.
```

Adapters are linked into the backend rather than loaded from a directory, so there is
nothing to install under `~/.sylph` and no setting naming one. When a vendor changes
faster than the shipped adapter, the fix is upgrading Sylph on that machine.

To see what actually resolved, and who supplied each value:

```sh
cd backend && cargo run -- doctor
```

Settings → Configuration in the app shows the same report.

An environment variable overrides the file — `SYLPH_HOME` relocates the whole
install, `SYLPH_PORT` the backend port. Full reference:
[**docs/configuration.md**](docs/configuration.md).

## Documentation

Comprehensive documentation lives in `docs/`. [**docs/README.md**](docs/README.md) is
the full index and says what belongs where; the entries below are the ones a first
reader wants.

- [**Documentation Index**](docs/README.md) — every document, and which one owns a
  given question
- [**Configuration**](docs/configuration.md) — `~/.sylph`, `config.toml`, and settings
- [**Development**](docs/development.md) — herdr deployment, health checks, troubleshooting
- [**Installation**](docs/installation.md) — Packaged install, upgrade, rollback
- [**Architecture**](docs/architecture.md) — System design, session model, identity
- [**Environments**](docs/environments.md) — Where a session's work runs
- [**Session Protocol**](docs/session-protocol.md) — The contract an adapter speaks
- [**Agents**](docs/agents.md) — Per-vendor shapes, quirks, and control surfaces
- [**Storage**](docs/storage.md) — Tiered history, replay, watermark-gated retention
- [**Frontend Architecture**](docs/frontend-architecture.md) — How the frontend is built
- [**UI Layout**](docs/ui-layout.md) — What the interface is: panes, surfaces, tokens
- [**Roadmap**](docs/roadmap.md) — Project status and what is next
- [**API**](docs/api/README.md) — REST endpoints and SSE streaming, one file per resource
- [**Database Schema**](docs/database-schema.md) — The journal, and the reasoning behind its shape

See [**AGENTS.md**](AGENTS.md) for the project's development rules and conventions.

## What It Does

**Runs sessions.** A session is a directory plus an agent. Creating one means naming
a directory — from the sidebar, the start screen, or a picker — not filling a form.
The composer exposes the model and reasoning effort each agent declares for itself,
so an agent that offers effort and no model list shows exactly that, and nothing is
hardcoded.

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

**Renders delegated work as a tree.** A subagent is a card inside the turn that spawned
it, to whatever depth the agent used, collapsed by default — and a failure three levels
down is reported on the collapsed card above it, so nothing has to be opened to learn
something broke.

**Reads history that predates it.** The sidebar groups sessions by the directory they
ran in, so a directory shows what agents did there before this app existed. Native
history is parsed by the same adapter that runs a live turn, into the same event
vocabulary, so there is one record and one search over it.

**Says how much resolution a record has.** `fidelity` is a property of the record —
`stream`, `blocks`, or `messages` — because a waterfall drawn over message-granularity
data would present equal-length bars as measurements. A surface reads it before drawing
a duration.

Deliberately absent, with reasons in [roadmap](docs/roadmap.md): session
delete/move/export, "Open in Default App" / "Reveal in Finder", and any non-loopback
access story.

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
