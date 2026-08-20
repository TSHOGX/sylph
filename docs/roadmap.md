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

**In progress:** Phase 4 (production readiness).

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
- ✅ TraceDB search integration — `GET /api/v1/trace/search`, `app/search/page.tsx`
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

## Phase 4: Production Readiness (Weeks 8-9)

**Goal:** Test coverage, accessibility, performance.

**Deliverables:**
- Unit tests (Rust backend, >80% coverage)
- Integration tests (API endpoints)
- E2E tests (Playwright, critical paths)
- WCAG 2.1 AA compliance audit
- Performance optimization:
  - Bundle size <200KB
  - LCP <2.5s
  - Virtual scrolling for large trajectories
- Documentation completion

**Non-functional:**
- Process crash recovery tests
- Concurrent access tests
- SSE reconnection stress tests
- Memory leak checks

## Phase 5: Deployment (Week 10)

**Goal:** Packaged release for local installation.

**Deliverables:**
- Binary packaging (`cargo build --release`)
- Frontend production build
- Data migration scripts
- Installation guide
- LaunchAgent configuration (macOS)
- Health checks and logging
- Rollback procedures

**Distribution:**
- Single binary for backend
- Static Next.js export (optional)
- Setup script for first run

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

Phase 5 exit criteria:
- Installation works on clean machine
- Migration from dev data succeeds
- LaunchAgent starts on boot
- Health checks report correctly
