# Sylph Web Development Guide

Sylph Web is a local-first native interface for Sylph and coding-agent sessions
archived by TraceDB.

This file holds rules and facts that stay true as the project moves. It
deliberately records no progress, phase, or status: `docs/roadmap.md` owns
those, and duplicating them here only creates a second copy to forget.

## Product Invariants

- Treat session origin as durable data. `sylph` sessions use Sylph identity and
  lifecycle policy; `native` sessions preserve the originating agent's identity
  and lifecycle.
- Never inject `SOUL.md`, reset, compact, or rewrite prompts for a native
  session unless the user explicitly converts it to a Sylph session.
- Read durable coding-agent history through TraceDB interfaces. Do not query
  TraceDB's SQLite schema directly.
- The live journal is authoritative only while a WebApp turn is active or has
  not yet appeared in TraceDB. TraceDB is authoritative for stable history.
- Bind to loopback by default. Any non-loopback deployment requires a separate
  authentication and threat-model change.

## Orientation

- `backend/` — Rust, Axum, SQLx. HTTP and SSE on port 4317 by default.
- `frontend/` — Next.js App Router, React, TanStack Query, Tailwind.
- `docs/` — the governing specs. Read the relevant section before implementing.
- `scripts/` — install and deployment tooling.

| Question | Authoritative source |
| --- | --- |
| What is built, what is next | `docs/roadmap.md` |
| System design, data flow | `docs/architecture.md` |
| Endpoint contracts | `docs/api-design.md`, `docs/api-v1.openapi.json` |
| Tables, indexes, migrations | `docs/database-schema.md` |
| UI structure and conventions | `docs/frontend-architecture.md` |

Record progress in commit messages and the roadmap. Do not add new progress,
handoff, or summary documents: they duplicate the roadmap, then drift from it.
Carry-forward context belongs in the roadmap's open items; a durable gotcha
belongs under Pitfalls below; reasoning about a specific decision belongs in a
comment beside the code.

## Running and Verifying

```sh
# backend
cd backend && cargo run
cd backend && cargo test && cargo clippy --all-targets

# frontend
cd frontend && npm run dev
cd frontend && npm run build && npx eslint . && npm run e2e
```

`npm run build` type-checks, so it is the TypeScript gate; there is no separate
`tsc` step.

E2E starts both servers itself on offset ports with a throwaway data directory
and a mock runner, so it never touches your real journal, archive, or agents.
Do not start servers by hand for it. It shells out to `cargo`, so the `PATH`
caveat below applies to E2E too, and a cold run compiles the backend first.

Run the checks for every part of the repo you touched, not only the one you
edited most. Backend and frontend share a wire contract, so a change on either
side can break the other's tests.

## Environment

| Variable | Applies to | Default |
| --- | --- | --- |
| `SYLPH_WEB_HOST` | backend | `127.0.0.1` |
| `SYLPH_WEB_PORT` | backend | `4317` |
| `SYLPH_WEB_DATA_DIR` | backend | `$HOME/.agents/data/sylph/web` |
| `SYLPH_RUNNERS_DIR` | backend | `$HOME/.agents/skills/sylph/runners` |
| `TRACEDB_BIN` | backend | `$HOME/Workspace/trace-db/target/release/trace-db` |
| `SYLPH_WEB_BACKEND` | frontend | `http://localhost:4317` |

The journal is always `live.db` inside the data dir; it is derived, not
separately configurable. `TRACEDB_BIN` defaults to a release build in a sibling
checkout rather than a `PATH` lookup, so TraceDB-dependent features degrade
until that binary exists — check `/api/v1/health` for `tracedb.available`.

## Pitfalls Worth Remembering

These have each cost real debugging time. They are properties of the toolchain
and the design, not of any current task.

**A proxy that gzips an SSE stream buffers it invisibly.** Next.js proxies
`/api/v1/*` through the standard `compression` middleware, which compresses any
compressible content type — `text/event-stream` included — and a gzip stream
cannot be decoded until its block is flushed. The whole turn then lands in the
browser at once, while `curl` sees perfectly paced frames because it sends no
`Accept-Encoding`. That asymmetry makes a transport bug look like a rendering
bug. The event stream therefore sets `Cache-Control: no-cache, no-store,
no-transform`; `no-transform` is what forbids the re-encoding, and it belongs on
the backend response so it holds behind any proxy, tunnel, or nginx.

**`EventSource` dispatches by frame name, so `onmessage` sees almost nothing.**
Every frame the backend emits is named after its event kind (`event: thinking`),
and `onmessage` receives only frames named `message` — so listening there drops
the entire stream. Per-kind `addEventListener` is not the fix either: a kind added
later is then silently dropped, and a frame named `error` collides with the
browser's own connection-error event. `frontend/lib/use-sse.ts` reads the body
with `fetch` and parses frames itself, which is also what makes the parser
unit-testable; `EventSource` is not, which is why the original bug shipped
untested.

**Testing "it streams" needs a runner slower than one render.** Every E2E spec
asserted final state against a mock runner that finished in well under a second,
so a client that dropped every mid-turn event passed the entire suite. The `paced`
scenario in `frontend/e2e/fixtures/runners/e2e/runner.sh` spaces its stages a
second apart, and `e2e/streaming.spec.ts` asserts an earlier stage is on screen
while a later one has not been emitted yet. Asserting only the end state cannot
distinguish streaming from batching.

**`cargo` is not on the default `PATH`.** Use `~/.cargo/bin/cargo`, or export
`PATH="$HOME/.cargo/bin:$PATH"` first. A bare `cargo test` fails with
`command not found`, which reads like a broken repo rather than a shell problem.

**SQLx runs queries at runtime here, not at compile time.** The codebase uses
`sqlx::query`, never the `query!` macros. There is no `DATABASE_URL` and no
`.sqlx` offline directory, so reaching for `query!` will not compile. SQL
mistakes surface in tests, which is why store-level tests matter.

**`next lint` was removed in Next 16.** Invoke ESLint directly (`npx eslint .`
or `npm run lint`). `npx next lint` fails with a misleading
`Invalid project directory` error naming a `lint` directory.

**`SYLPH_WEB_BACKEND` is read at build time.** Next compiles rewrites into the
route manifest, so it must be set for `next build`, not only for `next start`.
Setting it only at start time leaves a production build proxying to the default
port.

**Auth is a `SameSite=Strict` cookie, so the frontend must proxy `/api/v1/*`
same-origin.** A browser will not send the cookie cross-origin to another port.
The token rotates on backend restart, which invalidates open browser sessions by
design — do not "fix" it by persisting the token.

**Reading a mid-write file yields a phantom error.** With concurrent work in the
tree, a compiler or linter can capture a half-written file and report an error
whose line numbers do not match the source. Re-run before believing it.

**TraceDB's API spellings are inconsistent per-operation.** `show` emits
snake_case (`created_at_ms`, `started_at_ms`), while `stats`, `search`, and
`list` emit camelCase (`totalSessions`, `startedAtMs`). The backend client
(`backend/src/tracedb/mod.rs`) pins the exact spelling each operation emits so
a spelling change fails a test rather than silently yielding `None`. Do not add
dual-spelling aliases: they mask future renames and suppress parse errors that
would catch breaking changes.

## Spec and Code Evolution

Neither code nor spec wins automatically when they diverge. Stop, identify
whether behavior or intent should move, update the governing spec first when the
design changes, then update code.

Do not silently rewrite a spec to bless an implementation accident, and do not
contort code around a design that evidence has invalidated.

Before implementing:

1. Read the governing section in `docs/` and any linked open decision.
2. Verify behavior against source or a spike. Never turn an unverified
   assumption into an invariant.
3. If several materially different shapes remain viable, research reference
   repositories, dependency source, and prior art. Ask subagents for independent
   or contradictory reads when useful.

## Design Decisions

When a decision is uncertain, research it rather than guessing. That means
reading the dependency's source, studying how mature projects solved the same
problem, and measuring with a disposable spike where behavior is unclear.
Uncertainty is not permission to stall: investigate, then decide.

Aim for the best available answer across architecture, performance, UI and UX,
aesthetic judgment, code quality, and documentation quality — not merely the
first thing that works.

Prefer proven patterns and battle-tested libraries. Deviate when Sylph's own
requirements demand it, when profiling shows the standard pattern does not
address the bottleneck, or when security calls for something stricter. Document
the deviation and its evidence where the code lives.

Record decisions as comments next to the code they govern, including what was
rejected and why. A future reader needs the reasoning, not just the result.
`frontend/components/event-list.tsx` shows the intended density: it explains why
memoization was chosen over virtualization, and what evidence would reverse that
call.

Do not research basic language syntax, standard library usage, or trivial
implementation details.

## Compatibility

Before a live deployment, databases and local provider state are disposable:
change schemas and internal APIs in place, and do not add migrations or
compatibility branches for imagined users.

Once the project is deployed and carrying real data, migrations become mandatory
for schema changes. Treat the presence of installed state, not a date, as the
signal that this has flipped.

## Testing

Write tests for core business logic (session lifecycle, turn supervision, event
normalization), API endpoints (request validation, error responses, status
codes), and edge cases (concurrent access, process crashes, malformed input).

Name a test after the behavior it pins, so a failure reads as a broken promise
rather than a broken function. Assert on observable contract — status, error
code, ordering — not on incidental structure.

## Language

Code, documentation, and commit messages are English only: every `.rs`, `.ts`,
and `.tsx` file, everything in `docs/`, all comments, all identifiers and
endpoint names, all test names, and both commit subject and body.

Conversation happens in whatever language the user writes in. Durable artifacts
stay English regardless.

## Commits

Use Conventional Commits scoped to the affected component, and explain what
changed and why — plus how, when the approach is not obvious.

```
feat(backend): add SSE streaming for turn events
fix(db): correct session status transition logic
docs(arch): clarify runner discovery protocol
```

Stage files explicitly. Never `git add -A` or `git add .`, since unrelated
concurrent work may be sitting in the tree. Check `git status` and
`git diff --staged` before committing to confirm exactly what is included.

## Generated Agent Files

`frontend/AGENTS.md` and `frontend/CLAUDE.md` are written by `next dev`, not by
hand. The generator is
`frontend/node_modules/next/dist/server/lib/generate-agent-files.js`, and it
re-creates them when removed. Commit them with your work; reverting them from a
diff only produces the same uncommitted change again.

Their content is a real instruction, not boilerplate: this Next.js version has
breaking changes against older training data, so consult
`frontend/node_modules/next/dist/docs/` before writing frontend code.

The root `CLAUDE.md` is a symlink to this file. Edit `AGENTS.md`.
