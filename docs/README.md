# Sylph Web Documentation

Sylph Web is a local-first native interface for Sylph and coding-agent sessions.
The Sylph server holds the authoritative record of that history in its own store;
`docs/data-architecture.md` governs who owns which fact and which direction data
travels. This documentation describes the system design, implementation, and
development practices.

## Quick Navigation

### Getting Started
- **[Installation Guide](installation.md)** — Install and run Sylph Web locally
- **[Configuration](configuration.md)** — `~/.sylph`, `config.toml`, and moving an existing install
- **[Development Guide](development.md)** — Set up a development environment
- **[Roadmap](roadmap.md)** — Project status, completed phases, and future work

### Architecture & Design
- **[System Architecture](architecture.md)** — High-level system design, data flow, core concepts
- **[Frontend Architecture](frontend-architecture.md)** — how the frontend is built: state, streaming, performance
- **[UI Layout and Design](ui-layout.md)** — what the interface is: panes, surfaces, components, tokens
- **[API Design](api-design.md)** — REST API contracts and endpoint specifications
- **[Database Schema](database-schema.md)** — SQLite schema, migrations, data model

### Contracts & Subsystems
- **[Session Protocol](session-protocol.md)** — `sylph.agent.v3`: the contract between the server and a pluggable agent
- **[Storage](storage.md)** — Lossless tiered history, replay, and watermark-gated retention
- **[Data Architecture](data-architecture.md)** — Which copy is the truth, and which way data travels
- **[Control Plane](control-plane.md)** — Why one server owns session policy and frontends are transports
- **[Environments](environments.md)** — Where a session's work actually happens

### UX & Interface Design
- **[Agent UI Design Analysis](agent-ui-design-analysis.md)** — Why agent workspaces differ from chat UIs
- **[Keyboard Shortcuts](keyboard-shortcuts-default-sessions-settings.md)** — Shortcuts, Quick Start, Settings

## Documentation Principles

### What Belongs Where

| Question | Authoritative Source |
| --- | --- |
| What is built, what is next | [roadmap.md](roadmap.md) |
| System design, data flow | [architecture.md](architecture.md) |
| Endpoint contracts | [api-design.md](api-design.md) |
| Tables, indexes, migrations | [database-schema.md](database-schema.md) |
| How the frontend is built | [frontend-architecture.md](frontend-architecture.md) |
| What the interface looks like | [ui-layout.md](ui-layout.md) |
| How to install/deploy | [installation.md](installation.md) |
| Where files live, what a setting does | [configuration.md](configuration.md) |
| How to develop locally | [development.md](development.md) |
| Identity, origin, and `SOUL.md` | [architecture.md](architecture.md#session-model) |
| Translations and locale | [frontend-architecture.md](frontend-architecture.md#internationalization) |
| Which shortcuts a browser reserves | [ui-layout.md](ui-layout.md#keyboard-model) |
| What an adapter may send, and when | [session-protocol.md](session-protocol.md) |
| How history is kept, replayed, pruned | [storage.md](storage.md) |
| Who owns a fact, and which way it travels | [data-architecture.md](data-architecture.md) |
| Why the server owns session policy | [control-plane.md](control-plane.md) |
| Where a session's work runs | [environments.md](environments.md) |

### Design Decisions

When a choice involves trade-offs, the rationale lives **in code comments** next to
what it governs, not in separate decision logs. Examples:
- `frontend/components/timeline.tsx` explains why memoization was chosen over
  virtualization, and what evidence would reverse that call
- `frontend/lib/use-sse.ts` documents why `fetch` replaced `EventSource`
- `backend/src/tracedb/mod.rs` pins TraceDB API spellings and why
- `backend/src/paths.rs` states the containment invariant every write endpoint shares

This keeps decisions close to their consequences, where future maintainers encounter
them.

### Progress Tracking

**Record progress in commit messages and the roadmap.** Do not add new progress,
handoff, or summary documents: they duplicate the roadmap, then drift from it.
Carry-forward context belongs in the roadmap's open items; a durable gotcha
belongs in the code comments or a **Pitfalls** section; reasoning about a specific
decision belongs beside the code.

### Language

Code, documentation, and commit messages are English only: every `.rs`, `.ts`,
`.tsx` file, everything in `docs/`, all comments, all identifiers and endpoint
names, all test names, and both commit subject and body.

Conversation happens in whatever language the user writes in. Durable artifacts
stay English regardless.

## Key Design Principles

### 1. Origin as Policy
Session origin (`sylph` or `native`) is immutable and determines identity and
lifecycle. Native sessions preserve the originating agent's identity and never
receive prompt injection or resets.

### 2. Local-First
Data lives with the server that owns it, and no third-party service is in the path.
"Local" is a statement about dependencies rather than about geography: a Sylph
server deployed on a remote host is still local-first, because the authority is the
server's own store and not somebody else's API. `docs/architecture.md` § Design
Principles is authoritative; this is a summary of it.

### 3. The Server Owns the Record, for the Sessions It Spawned
An agent still writes its native format. For a **Sylph-born** session the server's
normalized event stream is the authority, because the server held the adapter's stdio
and is the one component present for all such work regardless of where it runs. For an
**adopted** session — one somebody else started, in a terminal — the server never saw a
stream, so the vendor's own store is the only record and the server's job is to read it
faithfully rather than to claim it. Fidelity and control are therefore properties of how
a session began, not of the product. `docs/data-architecture.md` § Two classes of
session is authoritative.

### 4. Agent Workspace, Not Chat App
The UI emphasizes tool execution over conversation. Tool calls are primary content
with prominent display; assistant prose is connective tissue; thinking is debugging
detail.

### 5. Performance Through Architecture
Maintain fast load times and responsive interactions through:
- Compact bundles via thoughtful dependency choices
- Code splitting for on-demand features
- Memoization and bounded rendering
- Lazy loading where appropriate

Monitor bundle size and render performance, but never let arbitrary limits constrain
valuable features or architectural improvements. Optimize deliberately; scale
proportionally to value delivered.

### 6. Accessibility by Default
WCAG 2.1 AA compliance maintained through automated testing (axe-core) and manual
verification. Keyboard navigation, screen reader support, and color-independent
indicators are requirements, not afterthoughts.

## Development Workflow

### Running Locally
```sh
# Backend
cd backend && cargo run

# Frontend
cd frontend && npm run dev
```

### Testing
```sh
# Backend
cd backend && cargo test && cargo clippy --all-targets

# Frontend
cd frontend && npm test              # Vitest unit tests
cd frontend && npm run build && npx eslint .
cd frontend && npm run e2e           # Playwright E2E tests
```

E2E starts both servers on offset ports with throwaway data and a mock adapter, so
it never touches your real journal or archive. It wipes its own home first, so do not
start servers by hand for it.

### Before Committing
1. Run tests for every part of the repo you touched
2. Stage files explicitly (never `git add .`)
3. Check `git diff --staged`
4. Write a descriptive commit message following Conventional Commits

## Document Evolution

Neither code nor spec wins automatically when they diverge. When implementation
differs from documentation:

1. Identify whether behavior or intent should move
2. If the design changed: update the spec first, then update code
3. If reality revealed the design was wrong: update spec to match reality and
   document why

Do not silently rewrite specs to bless implementation accidents. Do not contort
code around designs that evidence has invalidated.

## Design Documents vs Specs

Two files here are pre-implementation design arguments rather than descriptions of
the code: `agent-ui-design-analysis.md` and
`keyboard-shortcuts-default-sessions-settings.md`. They are kept because the
reasoning still governs the UI — the case against chat bubbles, and the shape of the
shortcut map — and each marks where the shipped implementation diverged from it.

Both now open with a status banner saying so, so a reader learns what kind of
document they are in before the first claim rather than at the end. Where a
divergence would mislead a reader who stops early, it is corrected in the body
instead of only in the closing errata — the shortcuts document's reserved bindings
(`Mod+Shift+O` and `Mod+J`, not the `Cmd+N` a browser takes first) and its Quick
Start path are both fixed inline for that reason.

When they disagree with `frontend-architecture.md`, `ui-layout.md`, or `roadmap.md`,
those win. A design document records what was intended; the architecture spec records
what is true. Do not update one to match the other without deciding which should move,
per "Document Evolution" above.
