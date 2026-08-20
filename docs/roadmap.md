# Sylph Web Roadmap

## Current State

**Architecture:**
- Backend: Rust + Axum + SQLx (Phase 1 complete)
- Frontend: Next.js 16 + React 19 + TanStack Query (Phase 2 complete)
- Database: SQLite (live journal) + TraceDB (stable history)

**Completed:**
- Phase 1: backend core on port 4317 — 11 routes, LiveStore, TurnSupervisor,
  EventHub, auth middleware, runner discovery, TraceDB client, integration tests
- Phase 2: frontend foundation — session list, create dialog, SSE conversation
  view, reconnection from `after_seq`
- Phase 3: feature completion — attachments, TraceDB search, native import,
  server-side filters, error boundaries. See `PHASE3_HANDOFF.md`.
- Phase 4: production readiness — 55 unit + 53 integration + 6 E2E tests,
  accessibility audit, first-load JS measured at 190.4 KB gzipped
- Phase 5: deployment — `scripts/install.sh`, standalone frontend bundle,
  LaunchAgent, reversible rollback, `docs/installation.md`

All five phases are complete. Open decisions and deferred work are recorded in
`PHASE5_HANDOFF.md`.

**Runner protocol:** the backend consumes `sylph.runner.v1` NDJSON lines
(`SYLPH_STREAM_FORMAT=ndjson-v1`) as specified in the Sylph skill's
`references/runner-protocol.md`. Event kinds: `thinking`, `assistant`,
`tool_call`, `tool_result`, `usage`, `status`; phases `started`/`delta`/
`completed`, where only `completed` is durable.

## Phase 1: Backend Core (Weeks 1-3) — Complete

**Goal:** Production-ready Rust backend on port 4317 with all API endpoints.

**Deliverables:**
- ✅ Database layer with migrations
- ✅ Session management (create, list, get)
- ✅ Health and diagnostics
- ✅ Turn management API
- ✅ Runner discovery via `--describe`
- ✅ Process supervision (TurnSupervisor)
- ✅ SSE streaming with reconnection
- ✅ Authentication middleware
- ✅ Integration tests

**API Endpoints:**
- `GET /api/v1/health`
- `GET /api/v1/diagnostics`
- `GET /api/v1/agents` + `POST /api/v1/agents/refresh`
- `GET /api/v1/sessions` + `POST /api/v1/sessions`
- `GET /api/v1/sessions/{id}`
- `POST /api/v1/sessions/{id}/turns`
- `GET /api/v1/turns/{id}`
- `GET /api/v1/turns/{id}/events` (SSE)
- `POST /api/v1/turns/{id}/stop`

## Phase 2: Frontend Foundation (Weeks 4-5) — Complete

**Goal:** Next.js UI with session list, conversation canvas, and SSE integration.

**Deliverables:**
- ✅ Next.js 16 App Router setup
- ✅ TypeScript strict mode
- ✅ API proxy routes (`/api/*` → Rust backend)
- ✅ Session list component
- ✅ Conversation timeline with message types
- ✅ SSE client with EventSource
- ✅ TanStack Query for server state
- ✅ Basic UI components (Tailwind)

**Pages:**
- `/` — Session list
- `/sessions/[id]` — Conversation view
- `/sessions/new` — Create session dialog

## Phase 3: Feature Completion (Weeks 6-7) — Complete

**Goal:** Full feature parity with specs.

**Deliverables:**
- ✅ Attachment upload (drag/drop, image previews) — `components/attachment-picker.tsx`
- ✅ Agent command dialogs (model/effort selection, status checks) — `components/command-dialog.tsx`
- ✅ TraceDB search integration — `GET /api/v1/search`, `app/search/page.tsx`
- ✅ Native session import flow — `POST /api/v1/sessions/import`
- ✅ Session filters (agent, workspace, time) — server-side, `LIMIT`-cap safe
- ✅ Error boundaries and retry logic — `lib/error-boundary.tsx`
- ✅ Reconnection handling for SSE — `after_seq` resume in `lib/use-sse.ts`

**UX:**
- ✅ Origin indicators (green for Sylph, violet for native)
- ✅ Preservation policy warning on native session resume
- ✅ Per-turn status (running, completed, failed)
- ✅ Copy/retry actions
- ✅ Scroll anchoring during streaming (pin-to-bottom only when already pinned)

**Carried forward:** the archive-metadata code path in `backend/src/tracedb/mod.rs`
is inert against the currently installed `trace-db` build, which returns a bare
event array from `show`. See `PHASE3_HANDOFF.md` for the divergence and the
rebuild decision.

## Phase 4: Production Readiness (Weeks 8-9) — Complete

**Goal:** Test coverage, accessibility, performance.

**Deliverables:**
- ✅ Unit tests — 55 in the Rust backend, up from 21
- ✅ Integration tests — 53 API-level, in-process through the real router
- ✅ E2E tests (Playwright, critical paths) — `frontend/e2e`, `npm run e2e`
- ✅ WCAG 2.1 AA audit — static audit plus browser-verified keyboard paths
- ✅ Performance:
  - ✅ First-load JS **190.4 KB gzipped** against the 200 KB budget
  - ✅ Event cards memoized so deltas do not re-render settled history
  - ⏸ Virtual scrolling deferred — a windowing library would consume most
    of the 9.6 KB of remaining budget, and profiling has not shown DOM
    size to be the bottleneck. Revisit if DOM count starts to dominate.
- ✅ Documentation — README first-run path, `PHASE4_HANDOFF.md`

**Non-functional:**
- ✅ Process crash recovery — restart reconciliation of in-flight turns
- ✅ Concurrent access — concurrent turns on one session conflict
- ✅ SSE reconnection — journal replay after reconnect, and after reload
- ⏸ Memory leak checks — needs a sustained-load harness, not yet built

**Not measured:** LCP. It needs a browser performance run against a
representative session rather than a synthetic page.

## Phase 5: Deployment (Week 10)

**Goal:** Packaged release for local installation. **Status: complete.**

**Deliverables:**
- ✅ Binary packaging — `cargo build --release`, a 7.3 MB single binary
- ✅ Frontend production build — `output: "standalone"`, a self-contained
  bundle carrying its own `node_modules`, so an install needs neither the
  repo nor `npm install` at runtime
- ✅ Data migration scripts — **not needed, deliberately.** sqlx runs the
  embedded migrations on startup and records them in the journal, so a
  development database carries into a packaged install untouched. Verified
  against a copy of the real dev journal.
- ✅ Installation guide — `docs/installation.md`
- ✅ LaunchAgent configuration (macOS) — `--launchagent`, with `RunAtLoad`
  and `KeepAlive`
- ✅ Health checks and logging — `/api/v1/health` unauthenticated for
  probes, `/api/v1/diagnostics` behind auth for counts and uptime; launchd
  logs rotate at 16 MB keeping one generation
- ✅ Rollback procedures — `--rollback`, which restores both the previous
  tree and its journal, and is itself reversible

**Distribution:**
- ✅ Single binary for backend
- ⏸ Static Next.js export — not done and not wanted. It was optional, and
  the app has a dynamic route (`/sessions/[id]`) plus a proxy rewrite to the
  backend, neither of which survives a static export.
- ✅ Setup script for first run — `scripts/install.sh`, which also handles
  upgrade, rollback, uninstall, and purge

## Future Enhancements (Post-Launch)

- **Session Management:** fork, rename, archive, export
- **Lineage:** reconstruct session history, show forks/branches
- **Permissions:** approval surfaces with audit log
- **Reconciliation:** TraceDB ingestion watermark tracking, duplicate pruning
- **Multi-user:** authenticated remote access (requires security redesign)
- **Performance:** trajectory virtualization for 1000+ events

## Release Criteria

Before each phase completion:
1. All deliverables implemented and tested
2. Documentation updated
3. Design decisions documented in relevant spec files
4. Demo to stakeholders
5. Commit history shows incremental progress

Phase 1 exit criteria:
- All `/api/v1/*` endpoints operational
- SSE streams reconnect correctly
- Process supervision handles crashes/timeouts
- Integration tests pass
- Backend can run standalone

Phase 2 exit criteria:
- UI renders sessions from backend
- Real-time updates via SSE
- Create session and send messages work
- Error states handled gracefully

Phase 3 exit criteria:
- Feature parity with specs
- Native session import preserves identity
- Search works across live + TraceDB sessions

Phase 4 exit criteria:
- >80% test coverage
- WCAG AA compliant
- Performance targets met
- Production-ready error handling

Phase 5 exit criteria — all met, with how each was checked:
- ✅ Installation works on clean machine — installed from a fresh `git clone`
  into a throwaway prefix, including `npm ci`, on non-default ports. Short of
  a VM this is the strongest available check: it proves the committed tree
  contains everything the build needs.
- ✅ Migration from dev data succeeds — a consistent read-only copy of the
  real development journal was opened by the packaged install, which reported
  schema version 1, served its sessions over the API, and logged no migration
  error.
- ✅ LaunchAgent starts on boot — `RunAtLoad` verified via `launchctl
  bootstrap`, the plist passes `plutil -lint`, and `KeepAlive` was confirmed
  to restart the pair after a `SIGKILL`. Not verified across an actual reboot.
- ✅ Health checks report correctly — `/api/v1/health` answers 200 without a
  cookie, directly and through the frontend proxy; `/api/v1/diagnostics`
  returns 401 without one.
