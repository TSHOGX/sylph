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

## Design Decision Process

**As Sylph Web enters continuous development, any uncertain design decision requires research:**

### When to Research

Research is required when you encounter:
- Architectural patterns (state management, error handling, caching strategies)
- Performance optimization (database indexing, query patterns, streaming protocols)
- Security considerations (authentication flows, input validation, rate limiting)
- User experience patterns (loading states, error recovery, accessibility)
- Protocol implementations (SSE reconnection, WebSocket fallbacks, HTTP retries)
- Third-party integrations (TraceDB protocol, runner discovery, external APIs)

**Do NOT research:** basic language syntax, standard library usage, or trivial implementation details.

### Research Workflow

1. **Define the problem clearly**
   - What are you trying to solve?
   - What are the constraints (performance, compatibility, security)?
   - What is the expected outcome?

2. **Search for established patterns:**
   - **GitHub:** repos with high stars in the relevant domain (Rust web frameworks, Next.js apps, SQLite patterns)
   - **Crates.io / npm:** popular, actively-maintained libraries
   - **Official documentation:** Rust docs, Next.js docs, Axum guides, SQLx examples
   - **Production implementations:** read source code of mature projects (e.g., how does Zed handle IPC? How does Linear handle optimistic updates?)

3. **Study mature open-source practices:**
   - How does Axum recommend structuring error types?
   - How does TanStack Query handle cache invalidation?
   - How does SQLite's own documentation recommend indexing for this query pattern?
   - How does TraceDB implement its JSON protocol?

4. **Document findings in code or specs:**
   - Add comments explaining why you chose approach X
   - Reference the source (link to GitHub repo, docs, article)
   - Note tradeoffs considered

5. **Implement based on proven patterns:**
   - Prefer battle-tested libraries over custom implementations
   - Adopt community conventions unless strong evidence suggests otherwise
   - Default to the simplest solution that meets requirements

### Example: Rust Backend Research

```rust
// Problem: How to handle database connection pooling?

// Research:
// 1. Read SQLx documentation on connection pools
// 2. Check Axum examples for pool management with state
// 3. Search GitHub for "axum sqlx pool" → find common patterns
// 4. Study realworld-axum-sqlx example repo

// Decision: Use SQLx's built-in Pool, shared via Axum's State
// Rationale: Proven pattern, minimal dependencies, good performance
// Reference: https://github.com/tokio-rs/axum/blob/main/examples/sqlx-postgres/src/main.rs

pub type DbPool = sqlx::SqlitePool;

pub async fn create_pool(database_url: &str) -> Result<DbPool> {
    SqlitePoolOptions::new()
        .max_connections(5)  // Conservative for single-user local app
        .connect(database_url)
        .await
        .map_err(|e| Error::Database(e.to_string()))
}
```

### Example: Frontend Research

```typescript
// Problem: How to handle optimistic updates with rollback?

// Research:
// 1. TanStack Query docs: "Optimistic Updates" section
// 2. GitHub: search "tanstack query optimistic rollback"
// 3. Real apps: how does Linear handle failed mutations?
// 4. Articles: Kent C. Dodds on optimistic UI patterns

// Decision: Use TanStack Query's onMutate + onError rollback
// Rationale: Built-in pattern, handles edge cases, well-tested
// Reference: https://tanstack.com/query/latest/docs/react/guides/optimistic-updates

const sendMessage = useMutation({
  mutationFn: api.turns.create,
  onMutate: async (variables) => {
    await queryClient.cancelQueries(['sessions', sessionId])
    const previous = queryClient.getQueryData(['sessions', sessionId])
    queryClient.setQueryData(['sessions', sessionId], /* optimistic state */)
    return { previous }  // Context for rollback
  },
  onError: (err, variables, context) => {
    queryClient.setQueryData(['sessions', sessionId], context.previous)
  }
})
```

### When NOT to Follow Established Patterns

Deviate from common patterns only when:
- Sylph's unique requirements (dual-origin sessions, TraceDB integration) demand it
- Performance profiling reveals a bottleneck the standard pattern doesn't address
- Security considerations require a stricter approach
- You have benchmarks showing measurable improvement

**Document the deviation clearly** — future maintainers need to understand why you went off the beaten path.

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
