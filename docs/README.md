# Sylph Web Documentation

Sylph Web is a local-first native interface for coding-agent sessions archived by
TraceDB. This documentation describes the system design, implementation, and
development practices.

## Quick Navigation

### Getting Started
- **[Installation Guide](installation.md)** — Install and run Sylph Web locally
- **[Development Guide](development.md)** — Set up a development environment
- **[Roadmap](roadmap.md)** — Project status, completed phases, and future work

### Architecture & Design
- **[System Architecture](architecture.md)** — High-level system design, data flow, core concepts
- **[Frontend Architecture](frontend-architecture.md)** — UI design, component structure, performance
- **[API Design](api-design.md)** — REST API contracts and endpoint specifications
- **[Database Schema](database-schema.md)** — SQLite schema, migrations, data model

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
| UI structure and conventions | [frontend-architecture.md](frontend-architecture.md) |
| How to install/deploy | [installation.md](installation.md) |
| How to develop locally | [development.md](development.md) |
| Identity, origin, and `SOUL.md` | [architecture.md](architecture.md#session-model) |
| Translations and locale | [frontend-architecture.md](frontend-architecture.md#internationalization) |
| Which shortcuts a browser reserves | [frontend-architecture.md](frontend-architecture.md#keyboard-model) |

### Design Decisions

When a choice involves trade-offs, the rationale lives **in code comments** next to
what it governs, not in separate decision logs. Examples:
- `frontend/components/event-list.tsx` explains why memoization was chosen over virtualization
- `frontend/lib/use-sse.ts` documents why `fetch` replaced `EventSource`
- `backend/src/tracedb/mod.rs` pins TraceDB API spellings and why

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
All data stored locally, no cloud dependencies. The system works offline and
respects user privacy by default.

### 3. Agent Workspace, Not Chat App
The UI emphasizes tool execution over conversation. Tool calls are primary content
with prominent display; assistant prose is connective tissue; thinking is debugging
detail.

### 4. Performance Through Architecture
Maintain fast load times and responsive interactions through:
- Compact bundles via thoughtful dependency choices
- Code splitting for on-demand features
- Memoization and bounded rendering
- Lazy loading where appropriate

Monitor bundle size and render performance, but never let arbitrary limits constrain
valuable features or architectural improvements. Optimize deliberately; scale
proportionally to value delivered.

### 5. Accessibility by Default
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

E2E starts both servers on offset ports with throwaway data and a mock runner, so
it never touches your real journal or archive.

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

Note one divergence that is easy to trip over: the shortcuts document proposes
`Cmd+N` and `Shift+Cmd+N` throughout, and a browser reserves both. The shipped
bindings are `Mod+Shift+O` and `Mod+J`. Its "What Shipped, and What Did Not" section
records this; `frontend-architecture.md` carries the current map.

When they disagree with `frontend-architecture.md` or `roadmap.md`, those win. A
design document records what was intended; the architecture spec records what is
true. Do not update one to match the other without deciding which should move, per
"Document Evolution" below.
