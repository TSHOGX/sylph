# Sylph Web Development Guide

Sylph Web is a local-first native interface for Sylph and coding-agent sessions archived by TraceDB.

## Product Invariants

- Treat session origin as durable data. `sylph` sessions use Sylph identity and lifecycle policy; `native` sessions preserve the originating agent's identity and lifecycle.
- Never inject `SOUL.md`, reset, compact, or rewrite prompts for a native session unless the user explicitly converts it to a Sylph session.
- Read durable coding-agent history through TraceDB interfaces. Do not query TraceDB's SQLite schema directly.
- The live journal is authoritative only while a WebApp turn is active or has not yet appeared in TraceDB. TraceDB is authoritative for stable history.
- Bind to loopback by default. Any non-loopback deployment requires a separate authentication and threat-model change.

## Development

**Backend (Rust)** — Phase 1 in progress:

```sh
cd backend
cargo run
```

**Frontend (Next.js)** — Phase 2 starts week 4:

```sh
cd frontend
npm run dev  # Coming soon
```

Rust 1.85+ is required. The backend uses SQLx for compile-time SQL verification and Axum for HTTP/SSE.

## Git Workflow

### Commits

Use Conventional Commits scoped to the affected component. Write both subject and body in English, even when conversation or changed files are in other languages.

```
feat(backend): add SSE streaming for turn events
fix(db): correct session status transition logic
docs(arch): clarify runner discovery protocol
chore(deps): update axum to 0.8.1
```

Commit messages must explain what changed and why. Include:
- What: the actual change made
- Why: the reason or context for the change
- How: approach taken if non-obvious

Bad: `fix bug`  
Good: `fix(db): prevent duplicate session creation on concurrent requests`

### Staging

Never use `git add -A` or `git add .` — stage files explicitly to avoid committing unintended changes:

```sh
git add backend/src/api/sessions.rs
git add docs/architecture.md
git commit
```

Use `git status` and `git diff --staged` before committing to verify exactly what will be committed.

### Branches

- `main` — stable, deployable code
- Feature branches for multi-commit work
- Push early, push often
- Squash or rebase before merging to main

## Spec and Code Evolution

Neither code nor spec wins automatically when they diverge. Stop, identify whether behavior or intent should move, update the governing spec first when the design changes, then update code.

Do not silently rewrite a spec to bless an implementation accident, and do not contort code around a design that evidence has invalidated.

Before implementation:

1. Read the governing section and any linked open decision.
2. Verify behavior against source or a spike. Never turn an unverified assumption into an invariant.
3. If several materially different shapes remain viable, research reference repositories, dependency source, and prior art. Ask subagents for independent and contradictory reads when useful; measure with a disposable spike where behavior is uncertain.

## Language Requirements

**Code, documentation, and commit messages: English only.**

- All `.rs`, `.ts`, `.tsx` source files
- All documentation in `docs/`
- All commit messages (subject and body)
- All comments in code
- API endpoint names, function names, variable names
- Test descriptions

**Chat and conversation: any language.**

The user may communicate in any language. Respond in the language they use. But all durable artifacts (code, docs, commits) remain in English for long-term maintainability and collaboration.

## Compatibility Before Deployment

Before a live deployment, databases and local provider state are disposable. Change schemas and internal APIs in place; do not add migrations or compatibility branches for imagined users.

Once deployed, migrations become mandatory for schema changes. Until then, iterate freely.

## Testing

Write tests for:
- Core business logic (session lifecycle, turn supervision, event normalization)
- API endpoints (request validation, error responses, status codes)
- Edge cases (concurrent access, process crashes, malformed input)

Run tests before committing:

```sh
cd backend
cargo test
cargo clippy
```

## Documentation Structure

- `AGENTS.md` (this file) — Development guide and project rules
- `README.md` — Project introduction and quick start
- `docs/architecture.md` — System architecture and design
- `docs/roadmap.md` — Product roadmap and release plan
- `docs/api-design.md` — API design principles and endpoints
- `docs/database-schema.md` — Database schema and migrations
- `docs/frontend-architecture.md` — Frontend architecture and UI design (Phase 2)

Progress is tracked through commit messages, not separate progress documents.

## Implementation Guidance

When implementing a feature:

1. Read the relevant spec section in `docs/`
2. Write or update tests first when useful
3. Implement the feature
4. Update documentation if behavior or design changes
5. Run tests and linters
6. Stage files explicitly and commit with a detailed message

When encountering ambiguity:
- Check existing documentation in `docs/` for design decisions
- Propose and document the decision before implementing
- Prefer simple, maintainable solutions over clever abstractions

## References

- Architecture: `docs/architecture.md`
- API specification: `docs/api-v1.openapi.json`
- Database schema: `docs/database-schema.md`
- Roadmap: `docs/roadmap.md`
