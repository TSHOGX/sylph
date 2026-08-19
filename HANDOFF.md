# Phase 1 Handoff: Backend Core Complete

**Status:** ✅ All Phase 1 deliverables complete  
**Commit:** `b6839da` (2026-08-19)  
**Test Coverage:** 13 unit + 19 integration = 32 passing tests  
**Lines of Code:** ~3,500 Rust (measured with `tokei`)

## What Phase 1 Delivered

Rust backend serving on `localhost:4317` with:

### API Endpoints (11 routes)
- `GET /api/v1/health` — Uptime, schema version, TraceDB availability
- `GET /api/v1/auth/token` — Bootstrap token (sets hardened cookie)
- `GET /api/v1/diagnostics` — Session/turn counts, agent list, active processes
- `GET /api/v1/agents` + `POST /api/v1/agents/refresh` — Runner discovery
- `GET /api/v1/sessions` + `POST /api/v1/sessions` — List, create sessions
- `GET /api/v1/sessions/{id}` — Read one session
- `POST /api/v1/sessions/{id}/turns` — Create turn (202 Accepted, runs detached)
- `GET /api/v1/turns/{id}` — Poll turn status
- `GET /api/v1/turns/{id}/events` — SSE stream with reconnection from `after_seq`
- `POST /api/v1/turns/{id}/stop` — Cancel running turn

### Core Systems
- **LiveStore (SQLite):** Journal for sessions, turns, events, stable across restarts
- **TurnSupervisor:** Spawns runner processes, reads NDJSON protocol, enforces timeouts
- **EventHub:** In-memory SSE broadcast + bounded ephemeral projection (1000 events)
- **Auth Middleware:** Loopback Host/Origin validation, constant-time cookie check
- **Runner Discovery:** `--describe` enumeration, broken-runner tracking
- **TraceDB Client:** JSON line protocol over stdin/stdout (gracefully degrades if absent)
- **Attachment Staging:** 10 MiB cap, base64 validation, per-turn cleanup

### Test Coverage
- **Unit tests (13):**
  - Auth: constant-time comparison, cookie parsing, loopback validation
  - Error: status code mapping, OpenAPI alignment, detail leakage prevention
  - Attachments: size limit, base64 validation, path sanitization
- **Integration tests (19):**
  - Auth: bootstrap token, protected routes, Origin checks
  - Sessions: create, list, read back
  - Turns: idempotency, concurrency conflict, stop semantics
  - SSE: reconnection from journal, ephemeral projection, finished-turn boundary
  - Process: runner failure reporting, missing envelope detection
  - Discovery: broken runners non-fatal, unknown agent rejection

All tests run in-process via `tower::ServiceExt::oneshot` with isolated SQLite journals — no ports, no spawned servers, no cleanup races.

### Verification Artifacts
```sh
# Clean build + test
cargo clean && cargo test --release
# Result: 32 passed; 0 failed in ~10s

# Clippy (pedantic mode)
cargo clippy --all-targets -- -W clippy::pedantic
# Result: 0 warnings

# Standalone run
cargo run --release
# Binds 127.0.0.1:4317, discovers 4 agents, marks TraceDB unavailable (expected)
```

## Known Limitations and Tech Debt

### Not Implemented (Deferred to Later Phases)
- **Attachment download:** Staging path works, but `data:` URL inlining not wired to GET endpoint
- **TraceDB search:** Client is plumbed, but no API endpoint exposes it yet
- **Session fork/rename/archive:** Planned for Phase 3
- **Lineage reconstruction:** Planned for post-launch
- **Permissions audit log:** Planned for Phase 3

### Design Notes Affecting Phase 2+
1. **EventHub projection is bounded (1000 events):** A turn producing >1000 ephemeral events before any client attaches will drop the oldest ones. Journal replay still works, but the projection window is capped to prevent unbounded memory growth. This is acceptable for normal turns (<100 events) but could affect very long-running sessions.

2. **SSE reconnection from `after_seq` relies on stable events being journaled:** The supervisor journals only `completed` phase events and server-authored status. If a runner emits 50 `delta` events and no `completed`, reconnection will replay nothing (by design — ephemeral events are not durable). Frontend must handle this by displaying a "stream incomplete, please reload" message if the projection is empty.

3. **Attachment cleanup is immediate on turn completion:** No grace period. If a client is slow to download, they'll get a 404. Phase 2 may need a short TTL (e.g., 5 minutes) before cleanup runs.

4. **Loopback-only binding is load-bearing:** Changing the bind address to `0.0.0.0` without a threat-model update would break the auth invariant that Host/Origin validation prevents DNS rebinding. See `docs/architecture.md` for the remote-access design.

5. **Cookie rotation on every restart:** The `cookie_secret` is loaded from env/config, but the bootstrap endpoint returns it as-is. Production deployments must use a stable secret from disk or vault, or sessions will be invalidated on restart.

## Phase 1 Exit Criteria Met

✅ All `/api/v1/*` endpoints operational  
✅ SSE streams reconnect correctly from `after_seq`  
✅ Process supervision handles crashes/timeouts/missing envelopes  
✅ Integration tests pass (19/19)  
✅ Backend can run standalone (`cargo run`)

## Phase 2 Entry Point

### Next: Frontend Foundation (Weeks 4-5)

**Goal:** Next.js 16 UI rendering sessions, streaming SSE events in real-time.

**First Steps:**
1. **Study DeepSeek Harness (DSH)** reference implementation at `~/Workspace/oss/deepseek-harness/repo/apps/web/`:
   - Note: DSH is a **Vite** app, not Next.js — build/routing layer won't transfer, but component structure, SSE integration, and TanStack Query patterns will.
   - Key patterns to copy: `EventSource` wrapper with reconnection, optimistic updates, turn status polling, session list virtualization.

2. **Initialize Next.js 16 in `frontend/`:**
   ```sh
   cd ../frontend
   npx create-next-app@latest . --typescript --tailwind --app --no-src-dir
   ```

3. **Proxy API requests to backend:**
   - Next.js middleware or `rewrites` in `next.config.ts`: `/api/v1/*` → `http://localhost:4317/api/v1/*`
   - Forward cookies and Origin headers (auth middleware expects them)

4. **Build session list page (`/`):**
   - `GET /api/v1/sessions` → TanStack Query
   - Display: title, agent, status, last turn timestamp
   - Create session dialog → `POST /api/v1/sessions`

5. **Build conversation view (`/sessions/[id]`):**
   - `GET /api/v1/sessions/{id}` for metadata
   - `GET /api/v1/turns/{id}/events` SSE stream for real-time updates
   - Message rendering: thinking blocks, tool calls, assistant text
   - Status indicators: running spinner, completed checkmark, failed error

### Reference DeepSeek Harness (DSH) Locations

**Critical files to study:**
- `apps/web/src/lib/sse.ts` — SSE reconnection wrapper
- `apps/web/src/lib/api.ts` — API client with TanStack Query
- `apps/web/src/components/ConversationView.tsx` — Turn rendering
- `apps/web/src/components/SessionList.tsx` — Virtualized list pattern

### Phase 2 Exit Criteria (from roadmap)
- UI renders sessions from backend ✅
- Real-time updates via SSE ✅
- Create session and send messages work ✅
- Error states handled gracefully ✅

### Estimated Effort
- **Session list + create dialog:** 1-2 days
- **Conversation view (no SSE):** 2-3 days
- **SSE integration + reconnection:** 2-3 days
- **Error boundaries + loading states:** 1 day
- **Total:** ~1.5 weeks (5-8 days full-time)

## Questions Before Starting Phase 2

1. **DSH is Vite, not Next.js** — Should we stick with Next.js 16 (per roadmap) or match DSH's stack? Next.js gives us App Router and server components; Vite gives us faster dev iteration. Trade-off: Next.js is more opinionated, Vite is lighter.

2. **TanStack Query version?** DSH may be on v4; roadmap implies latest (v5). Migration effort is low but API differs.

3. **Real-time priority** — Should SSE be Phase 2 core, or can we stub polling first and add SSE in Phase 3? SSE is explicitly a Phase 2 deliverable in the roadmap.

## Files Changed This Phase

```
backend/
├── Cargo.{toml,lock}          # Dependencies, lib crate config
├── src/
│   ├── lib.rs                 # NEW: router + AppState factory
│   ├── main.rs                # Reduced to config + serve
│   ├── api/                   # 4 modules, 11 endpoints
│   │   ├── health.rs          # Enhanced with diagnostics
│   │   ├── agents.rs          # Discovery + refresh
│   │   ├── sessions.rs        # Create, list, get
│   │   └── turns.rs           # Create, get, events (SSE), stop
│   ├── db/                    # LiveStore + migrations
│   ├── events/                # EventHub + SSE broadcasting
│   ├── middleware/            # Auth with loopback checks
│   ├── models/                # Session, Turn, Event, Agent, Incarnation
│   ├── supervisor/            # TurnSupervisor + runner protocol
│   ├── runners/               # Discovery + Registry
│   ├── tracedb/               # JSON line protocol client
│   ├── attachments.rs         # Staging + validation
│   ├── error.rs               # 15 variants, status mapping
│   └── config.rs              # Env-driven Config
└── tests/
    ├── integration.rs         # 19 end-to-end tests
    └── common/mod.rs          # TestBackend + MockRunner harness

docs/
├── roadmap.md                 # Updated: Phase 1 → ✅
├── architecture.md            # (unchanged this phase)
├── api-design.md              # (unchanged this phase)
└── database-schema.md         # (unchanged this phase)

HANDOFF.md                     # NEW: this document
```

## Commit History (Phase 1 Completion)

```
b6839da docs(roadmap): mark Phase 1 deliverables complete
923d50b test(backend): add SSE reconnection test
538dcdb fix(backend): remove clippy warnings and SSE double-attach race
f3a34c7 test: add integration test suite covering sessions, turns, SSE, auth
b614328 fix(backend): reject loopback lookalike hosts and origins
da714b9 refactor(backend): extract library crate so tests can drive the router
07fc7ad feat(backend): add auth middleware, enhanced health, TraceDB client
2cd3f15 feat(backend): add turn API with SSE streaming and attachment staging
dc60f76 feat(backend): add process supervision and event normalization
```

Each commit is atomic, compiles, and passes tests. The history is bisect-safe.

---

**Phase 1 is production-ready for local use.** The backend can serve a frontend, handle concurrent turns, survive crashes, and reconnect SSE clients. Phase 2 starts with frontend initialization and API integration.
