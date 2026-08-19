# Sylph Web product design

## Interaction model

The interface follows DeepSeek Harness's strongest pattern: a compact session rail, one focused conversation canvas, visible tool/reasoning events, and a composer that remains anchored while the trajectory scrolls. Sylph adds an origin distinction that DeepSeek Harness does not need.

Green indicates a Sylph-managed session. Violet indicates an agent-native session. Every native conversation shows the preservation policy before the user resumes it. This avoids an invisible identity change at the highest-risk moment.

## Release slices

### Phase 1 — Backend core (Weeks 1-3, in progress)

- Rust Axum HTTP server + Tokio runtime
- SQLite via SQLx with compile-time query verification
- Numbered migration system with transactional application
- `/api/v1` REST endpoints: health, diagnostics, agents, sessions, turns
- SSE streaming for turn events with reconnection and replay
- Runner discovery via `--describe`
- Process supervision with TurnSupervisor
- Authentication middleware (loopback cookie)
- Integration test suite

### Phase 2 — Frontend foundation (Weeks 4-5, planned)

- Next.js 16 App Router + React 19 Server Components
- TypeScript strict mode with shared type definitions
- Session list and conversation canvas
- SSE integration via EventSource
- TanStack Query for server state
- Basic UI components (Radix UI + Tailwind)

### Phase 3 — Feature alignment (Weeks 6-7, planned)
- Attachment upload with drag/drop and image previews
- Agent-specific model/effort controls from `--describe`
- Agent command dialogs (replacing slash-command memorization)
- Full-text TraceDB search with agent/workspace/model/time filters
- Native session import and continuation
- Error boundaries and retry mechanisms

### Phase 4 — Production readiness (Weeks 8-9, planned)

- Comprehensive test suite (unit, integration, E2E)
- Accessibility audit and WCAG 2.1 AA compliance
- Performance optimization (bundle, SSE, DB queries)
- Documentation (architecture, API, deployment)
- Developer tooling (hot reload, debugging)

### Phase 5 — Deployment (Week 10, planned)

- Binary packaging (`cargo build --release`)
- Frontend production build
- Data migration scripts and testing
- LaunchAgent configuration (macOS)
- Health checks and monitoring
- Rollback procedures

## Legacy implementation (removed)

The original Node.js + vanilla JS prototype has been removed. All references to `.mjs` files, `npm start`, and `src/api-types.d.ts` are obsolete.

## Future enhancements
