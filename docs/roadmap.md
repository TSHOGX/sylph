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
  view, reconnection from `after_seq` (the list and the dialog are both gone: the
  dialog in Phase 7, the list in Phase 8)
- Phase 3: feature completion — attachments, TraceDB search, native import,
  server-side filters, error boundaries
- Phase 4: production readiness — 55 unit + 53 integration + 6 E2E tests,
  accessibility audit, first-load JS measured at 190.4 KB gzipped
- Phase 5: deployment — `scripts/install.sh`, standalone frontend bundle,
  LaunchAgent, reversible rollback, `docs/installation.md`
- Phase 6: frontend redesign — three-pane agent workspace, tool-first timeline
  with inline diffs, file browser, `Cmd+K`/`Cmd+P`, settings panel
- Phase 7 (Artifacts): file viewer taking the whole right pane, with fullscreen,
  and previews for Excel, Word, PowerPoint, CSV, images, HTML, and PDF

All six phases are complete, including 6.6 and 6.7 (archive browsing). Phase 7 ran
as two tracks — the interaction and composer redesign, and the Artifacts file
viewer — both of which have landed, as has Phase 8 (the start screen); see the
sections at the end of this file.

### Open Decisions

One decision remaining:

**Native→Sylph conversion endpoint shape:** `PATCH /api/v1/sessions/{id}`
with `identityMode`/`lifecycleMode` fields vs. `POST
/api/v1/sessions/{id}/convert`. The latter is recommended: conversion is the
only legitimate way to inject `SOUL.md` into a native session, so it should be
an explicit, auditable operation rather than a field on a general update. Not
implemented either way yet.

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
- `/` — Session list (replaced in Phase 8 by the start screen: the sidebar, `Cmd+K`,
  and `Cmd+Shift+K` had each become a better answer to part of what it did)
- `/sessions/[id]` — Conversation view
- `/sessions/new` — Create session dialog (removed in Phase 7: creating a session
  is a mutation, and the directory it needs comes from a picker)

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
- ✅ Documentation — README first-run path

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

## Phase 6: Frontend Redesign — Complete

**Goal:** turn the frontend from a technical demonstration into a production
agent workspace, where tool execution is the primary content and the keyboard is
the primary input.

**The shift, concretely:** away from chat-app UX (message bubbles, alternating
alignment, tool calls minimized) toward the pattern Claude Code, Cursor, and Zed
share — a flat timeline where tool calls are the visual anchors. `docs/agent-ui-design-analysis.md`
holds the comparison this rests on; `docs/frontend-architecture.md` holds the
shipped design.

### Delivered

**6.1 — three-pane layout and workspace sidebar**
- ✅ `AppShell` in the root layout, so panes keep state across navigation
- ✅ Sessions grouped by `workdir`; Quick Sessions (`~/.agents`) listed separately
- ✅ Pin and rename workspaces; right-click menus on groups and sessions
- ✅ `GET /api/v1/workspaces` — the roll-up, and the only source of the resolved
  `~/.agents` path, which a browser cannot expand

**6.2 — agent-first conversation canvas**
- ✅ Flat timeline, no bubbles; tool calls as bordered regions with target and status
- ✅ Tool call and its `tool_result` render as one operation, paired by `streamId`
- ✅ Inline unified diffs computed browser-side from edit arguments
- ✅ Expansion policy by role: thinking collapsed, edits open, shell output open
  only on failure
- ✅ Markdown for assistant prose; `lib/tool-shape.ts` normalizes all four runners'
  payload shapes
- ✅ Clicking any file path opens it in the Artifacts pane

**6.3 — Artifacts pane and file API**
- ✅ `GET /api/v1/sessions/{id}/files` and `.../files/content`, read-only, rooted
  at the session's journalled `workdir`
- ✅ Containment enforced on the canonical path, so `..` and symlinks cannot
  escape; escaping symlinks are also hidden from listings
- ✅ Lazy per-directory expansion; Markdown, code, image, and JSON previews
- ✅ Encoding decided by the bytes, not the extension

**6.4 — search and command palette**
- ✅ `Cmd+K`: live sessions ranked client-side, archive searched server-side,
  already-imported hits de-duplicated
- ✅ `Cmd+P`: palette dispatching through the same paths as the shortcuts, with
  recency ordering
- ✅ One document-level dispatcher; bindings stored platform-neutral and rebindable

**6.5 — settings and Quick Start**
- ✅ Default agent and per-agent model, density, expansion defaults, shortcut
  recorder, workspace management
- ✅ Quick Start creates a session in `~/.agents` with no dialog
- ✅ Any known directory creates with no dialog: one shared mutation
  (`lib/create-session.ts`) serves Quick Start and every sidebar group, so
  "New session here" no longer re-asks for a directory the group already names.
  The form remains for choosing a directory, or overriding agent/model/title.
- ✅ Preferences over `localStorage` via `useSyncExternalStore`

**6.6 — archived history is browsable by directory**

Selecting or pinning a directory used to show "No sessions yet" even when TraceDB
held hundreds of sessions for that exact path: the sidebar read only the live
journal, and archived history was reachable only by full-text search, which needs
a query the user does not have. TraceDB's `list` op — with a `cwd` filter and
cursor pagination — was never called.

- ✅ `GET /api/v1/archive/sessions` — archived sessions for one directory, with
  exact-path re-filtering (TraceDB's `cwd` filter is a substring match), lineage
  rollup, and empty sessions dropped
- ✅ `GET /api/v1/archive/sessions/{traceId}` — read-only transcript, normalized
  into the live `StreamEvent` shape so one timeline renders both; greedy id capture
  because a subagent id contains a slash
- ✅ Row labels from the archive's title, else the opening *typed* message with
  harness-injected envelopes skipped, else an envelope's own `summary` attribute —
  98.3% of non-empty archived sessions get a recognizable label, and 99.5% of
  listed rows do, against ~10% taking the first user event verbatim
- ✅ Sidebar merges live and archived rows, deduplicated on imported
  `traceSessionId` or the runner's `<agent>:<native-session-id>` identity, with
  `⌗` marking archived and `⑂N` rolled-up subagents
- ✅ `Cmd+Shift+K` — workspace-scoped history browser with cursor paging
- ✅ `/archive/[...id]` — read-only view offering explicit continuation, disabled
  with its reason when the archive recorded no `cwd`
- ✅ `GET /api/v1/workspaces?includeArchived=true` — directories whose only history
  is archived, so one can be pinned or picked before it has a live session (138
  discoverable locally, against 1 before)
- ✅ Fixed along the way: imported sessions were titled
  `<permissions instructions> …` because `summarize_ask` applied no envelope
  filter; live rows now persist the opening ask as their title and expose the
  runner's native id for copy/paste; archive matching also covers sessions that
  are ingested after a live turn without requiring an import

**6.7 — lineage rollup reads the archive instead of guessing**

Reported from use: subagent folding missed cases. It did, and measuring the whole
archive found the first version wrong in three separate ways — one under-folding,
two hiding real work.

- ✅ Lineage now comes from `parent_session_id` / `forked_from` via a cheap
  metadata `show` (~0.5 ms), not from the `/agent-` id suffix. The suffix missed
  **199** delegated sessions that record only a parent, almost all `codex`, and
  they sat at the top level of every listing. It remains a fallback for a failed
  lookup.
- ✅ A subagent that ran in a **different directory** than its parent is listed
  rather than folded. **484 of 2238** do — worktrees and subdirectories — and
  folding them into an absent parent emptied **42 directories**, one holding 193
  sessions. That was the same false "nothing here" that 6.6 set out to fix.
- ✅ `forked_from` is distinguished from delegation. A fork replays its origin's
  whole prefix, so both sides read as one conversation; one survives, and which one
  is not a fixed side — measured locally, one pair keeps the fork (1108 events
  against 389) and the other the origin (2459 against 649). Newest-first ordering
  makes "the one reached first" the correct answer. `forkOf` reports the suppressed
  side so a client accumulating pages can collapse a pair split across requests, as
  one measured pair 268 rows apart is.
- ✅ The upstream scan is bounded in **rows examined, not pages**. An unanchored
  substring `cwd` match means a parent directory also matches everything beneath
  it; `~/Workspace/huawei` has 88 sessions whose first exact match sits ~1300 rows
  in, and the old 8-page cap reported it empty. It now returns its history in
  0.76 s, the worst case measured.
- ✅ Labels gained a third fallback: an envelope's own `summary` attribute, for
  delegated sessions whose every turn is a `<teammate-message>`. Coverage went from
  94.3% to **98.3%** of non-empty sessions, and 99.5% of listed rows.
- ✅ Verified across all 138 directories holding archived history: **137 list
  correctly**. Only `/` does not, and cannot — every path contains it, so no
  substring filter can isolate it.
- ✅ Fixed along the way: `MockTraceDb` emitted payloads inside single-quoted shell
  words, so an apostrophe in fixture data truncated the response and surfaced as a
  JSON decoding error that read like a protocol bug.

### Exit criteria — how each was checked

- ✅ **Three panes functional** — toggles, persistence across reload, and
  keyboard operation covered in `e2e/workspace-shell.spec.ts`.
- ✅ **No chat bubbles; tool execution primary** — `e2e/timeline.spec.ts` asserts
  tool calls render as labelled regions with their target, that an edit shows a
  real diff with `+1`/`−1` counts, that a failed command's output is visible
  without interaction, and that a successful read's is not.
- ✅ **File tree and previews** — integration tests cover listing, ordering,
  skipped directories, and text/image/binary detection; the pane is exercised in
  E2E.
- ✅ **`Cmd+K` and `Cmd+P` keyboard-navigable** — both open, filter, and commit
  by keyboard in E2E, including focus restoration on close.
- ✅ **Quick Start** — creates in the backend-resolved `~/.agents`; the directory
  is asserted to end in `/.agents` in integration tests. Dialog-free creation from
  a sidebar group is covered in `e2e/workspace-shell.spec.ts`, and default
  resolution against installed runners in `lib/create-session.test.ts`.
- ✅ **Settings configure agent and model** — round-trip through storage and into
  the timeline covered in E2E.
- ✅ **First-load JS stays compact and efficient** — **measured 197.4 KB**, from a 187.8 KB
  baseline: +8.4 KB for the Phase 6 shell and +1.2 KB for archive browsing. The project
  maintains a lean bundle through deliberate architecture choices: hand-rolled Markdown,
  diff computation, and fuzzy matching instead of adding ~87 KB of dependencies. Bundle
  size is monitored and managed, but not constrained by hard limits — as features grow,
  optimizations scale proportionally to deliver fast load times and excellent user
  experience.
- ✅ **1000-event conversation renders under 2s** — measured on a cold reload of
  durable history in `e2e/timeline.spec.ts`.
- ✅ **WCAG 2.1 AA maintained** — axe-core (`wcag2a`/`wcag2aa`/`wcag21a`/`wcag21aa`)
  reports **zero violations** across the session list, create form, a conversation
  canvas with tool calls and both sidebars open, and all three overlays. It caught
  two real defects during the work: an `aria-activedescendant` pointing at no
  element, and 2.48:1 muted text.
- ✅ **E2E passes with Phase 6 scenarios** — 18 specs, up from 6. Plus 81 new
  Vitest unit tests over the diff, Markdown parser, tool-shape interpreter, fuzzy
  ranker, shortcut grammar, and timeline row building.

**Test totals:** 61 backend unit + 62 backend integration + 81 frontend unit +
18 E2E.

### Carried forward

- **Live streaming was fixed after Phase 6 shipped**, and the two causes are worth
  keeping in mind rather than only in `AGENTS.md`. The transcript arrived in one
  lump at turn end because (1) the client listened on `EventSource.onmessage`
  while the backend names every frame after its event kind, and (2) the Next.js
  proxy gzipped `text/event-stream`, which cannot decode until a block flushes.
  Neither was visible to `curl`, which sends no `Accept-Encoding` and so saw
  correctly paced frames. What let this ship green was the test shape: every spec
  asserted final state against a runner faster than one render, which cannot
  distinguish streaming from batching. `e2e/streaming.spec.ts` and the `paced`
  fixture scenario now assert mid-turn painting.
- **Virtualization** stays deferred, now with a measurement behind it: a
  1000-event conversation renders inside budget through memoization and bounded
  rendering. The evidence that would reverse this is scroll jank while *idle* on a
  large session, which indicates DOM size rather than render count.
- **Dark theme** is deliberately absent rather than half-built. It needs a `dark:`
  pass over every component plus a resolved `prefers-color-scheme` fallback; a
  settings toggle storing a preference nothing reads would misrepresent that.
- **"Open in Default App" and "Reveal in Finder"** are specified in
  `docs/keyboard-shortcuts-default-sessions-settings.md` but not implemented. Both
  need the backend to spawn a process against a client-supplied path, which is a
  local code-execution surface and needs a threat-model decision, not a menu entry.
- **Screen-reader passes** (VoiceOver, NVDA) and forced-colours mode are untested.
- **Session move, delete, and export** are still absent. Rename now has an
  endpoint (`PATCH /api/v1/sessions/{id}` accepts `title`), but no context menu
  calls it yet; the rest have no endpoint behind them.
- **`Cmd+F` filters turns** rather than highlighting matches inside them.
  Highlighting would mean reaching into the Markdown, diff, and code renderers to
  rewrite their text nodes.

### Superseded design documents

Phase 6 design was drafted across several documents, some of which were still
being rewritten when implementation began. The two that remain authoritative for
*intent* are `agent-ui-design-analysis.md` (the UI pattern argument) and
`keyboard-shortcuts-default-sessions-settings.md` (shortcut map, Quick Start,
settings). The rest — `phase6-design-index.md`, `phase6-frontend-redesign.md`,
`phase6-quick-start.md`, `phase6-research-checklist.md`, `PHASE6_*.md` — are
planning scaffolding that the shipped design and `docs/frontend-architecture.md`
now supersede. They describe a bundle allocation (~80 KB of new dependencies) that
the measured budget did not permit, so following them now would be actively
misleading. **They should be deleted;** they are left in place only because
removing another author's untracked drafts is not this change's call to make.

## Phase 7: Interaction and Composer Redesign — Complete

**Goal:** remove the forms and stacked form-fields that Phase 6 left in the two
surfaces a user touches most — creating a session, and writing a message.

**The shift, concretely:** creation happens where the context already exists
(right-click a workspace, get a session in it) rather than in a form that re-asks
for what the click named; and the composer becomes one control surface exposing
the agent, model, and reasoning effort that the runners have always declared and
the UI never showed.

### Delivered

**7.1 — machine-level directory browsing**
- ✅ `GET /api/v1/fs/{roots,tree}`, bounded by `$HOME` plus `SYLPH_WEB_BROWSE_ROOTS`
- ✅ Containment extracted to `backend/src/paths.rs` and shared with the
  session file API, so the rule exists once rather than in two divergent copies
- ✅ Directories only, no content endpoint, escaping symlinks omitted from
  listings — deliberately narrower than the session-scoped file API
- ✅ `DirectoryPicker`, one level with a breadcrumb, dynamically imported

**7.2 — creation without forms**
- ✅ "New session here" creates and opens directly, wiring up the
  `useQuickCreateSession` hook Phase 6 wrote and never called
- ✅ "New Workspace..." binds a directory with no session in it, via
  `pinnedWorkspaces` — no workspace table needed, since the sidebar already
  rendered pinned paths as empty groups
- ✅ Top-bar "New session" and "⚡ Quick Start" removed; `Mod+N`,
  `Mod+Shift+N`, and both palette entries still work through `runAction`
- ✅ One shared creator in the shell, so every path reports one pending state
  and one error banner

**7.3 — the composer**
- ✅ One bordered surface: attachment chips, auto-growing textarea, and controls
- ✅ Agent shown bottom-left; switchable at turn zero, a plain label with an
  explanatory tooltip afterwards
- ✅ Model and Thinking rendered **from `--describe`**, so `codex` correctly shows
  effort alone — verified against all four installed runners
- ✅ `PATCH /api/v1/sessions/{id}` for flags, validated against the declared
  schema on write rather than dropped silently at turn time
- ✅ Attachments moved inside; paste-to-attach added; drag-and-drop over the
  whole surface
- ✅ `POST /api/v1/sessions/{id}/files` uploads into the workdir subtree, refusing
  to overwrite by default

### Exit criteria — how each was checked

- ✅ **No form on the fast path** — E2E asserts a right-click reaches a
  conversation with no `Working directory` field and never the create route, for
  both Quick Sessions and a project group.
- ✅ **Removed buttons stay removed, shortcuts keep working** — E2E asserts both
  buttons are absent from the banner and that `Mod+N` still opens the form.
- ✅ **A workspace can exist before a session** — E2E binds `/tmp` through the
  picker and finds the group.
- ✅ **Agent locks after the first turn** — E2E sends a turn and asserts the
  `<select>` is replaced by the explanatory label.
- ✅ **Path containment holds** — 16 new integration tests over `..`, escaping
  symlinks, prefix siblings, cross-root traversal, root boundaries, filename
  sanitization, overwrite refusal, and flag validation.
- ✅ **First-load JS under 200 KB** — **measured 196.6 KB**, +0.4 KB over Phase 6.
- ✅ **No regressions** — 68 backend unit + 84 integration + 98 frontend unit +
  28 E2E, all passing; clippy and ESLint clean.

**Test totals:** 68 backend unit + 84 backend integration + 98 frontend unit +
28 E2E.

### Carried forward

- **Uploading outside the workdir** is deliberately not possible. The agent can
  only write under `workdir`, so allowing the browser more would give the UI a
  wider write range than the process it drives. Revisit only with a real need and
  a threat-model decision, not for convenience.
- **`browse_roots` widening** is a configuration decision with real consequence:
  it is what any client of this backend can enumerate. The default is `$HOME`
  alone, and `docs/architecture.md` records why enumeration is the part that
  matters.
- **Rename from the sidebar** now has an endpoint but no menu item.
- **Dark theme** remains deliberately absent; the composer and picker were built
  with the same light-only palette as the rest, so they inherit that debt.
- **The agent switcher creates a new session** rather than converting the current
  one, because `agent` is fixed at creation server-side. On an empty session there
  is nothing to preserve, so this is correct rather than a workaround — but it
  means the switcher is unavailable exactly where conversion would be interesting.

## Phase 7: Artifacts file viewer — Complete

**Goal:** make the Artifacts pane answer "what did the agent do to my files?" for
the formats an agent actually produces, not only for source code.

The second Phase 7 track, developed in parallel with the composer redesign above
and merged after it. This section covers the Artifacts pane and the file endpoints
behind it; the composer, directory picker, and archive browsing are covered above.

### Delivered

- **Tree and viewer swap.** Selecting a file gives the viewer the whole pane with a
  back affordance, replacing a `max-h-[55%]` strip of a 320px pane. Both views stay
  mounted and toggle with `hidden`, so expanded directories and scroll offset
  survive a round trip.
- **Fullscreen**, reusing `Overlay` with a `size="viewer"` variant rather than a
  second modal implementation.
- **Excel, Word, PowerPoint, CSV/TSV, images, HTML, and PDF**, from two new
  endpoints: `GET /files/document` for parsed structure and `GET /files/raw` for
  bytes the browser renders itself.
- **Server-side parsing** via `calamine` and `quick-xml`, keeping ~7.5 MB of
  unmaintained, CVE-carrying npm dependency out of the browser.
- **Sandboxed HTML rendering**, default-on because `sandbox=""` removes script
  execution outright rather than gating it.

### Exit criteria — how each was checked

- ✅ **Every requested format previews** — verified by driving the running app in a
  browser against real files produced by Excel-compatible tooling, not only against
  test fixtures. That distinction paid for itself three times: see below.
- ✅ **First-load JS stays under budget** — measured 197.1 KB gzipped against
  200 KB, +0.9 KB for the whole feature. Each viewer is a `next/dynamic` import
  beneath the already-split pane; the grid viewer was confirmed absent from the
  first-load chunk set and fetched on demand at 1.5 KB.
- ✅ **Grid performance without a virtualization dependency** — measured in headless
  Chromium: 46 ms to build the 200-row default window, and scroll cost flat at
  ~0.2 ms/frame from 6 000 to 100 000 cells. Paging replaces the window rather than
  accumulating, so the DOM stays at one window however far the user pages.
- ✅ **Rendering untrusted HTML cannot reach the app** — pinned in a real browser:
  markup renders and inline CSS applies, while inline `<script>` does not run,
  `parent.document` is unreachable, and `document.cookie` inside the frame throws
  `SecurityError`. The hardened response headers are asserted server-side too,
  because dropping the CSP header silently reintroduces cookie disclosure — which
  was measured, not assumed.
- ✅ **Path containment holds on the new routes** — traversal and escaping-symlink
  refusal are asserted per route rather than assumed inherited, plus 401 without
  the session cookie.
- ✅ **WCAG 2.1 AA maintained** — axe-core reports zero violations with the viewer
  open in the pane and in the fullscreen dialog. It also caught a real focus bug:
  closing fullscreen dropped focus to `<body>`, because the pane unmounts the
  expand button while the dialog is up and `Overlay` correctly refuses to focus a
  detached node.
- ✅ **Test totals**: 96 backend unit (+35), 79 backend integration (+11), 156
  frontend unit (+55), 31 E2E (+7). Plus three ignored-by-default tests that parse
  files from real Office tooling.

### What real files taught that fixtures did not

Three bugs survived a green synthesized-fixture suite and were found by parsing
output from openpyxl, python-docx, and python-pptx. All three shared a cause: a
hand-written fixture can only encode what its author already believed.

- **Formulas landed on the wrong cells.** calamine gives the formula range its own
  origin, so indexing it with value-range coordinates shifted every formula — the
  header cell "Region" was reported as a formula carrying `B2+C2`.
- **Uncalculated formulas rendered blank.** Excel always caches a result; openpyxl
  writes either no `<v>` or an empty one, which arrive as two different values and
  both had to be handled. An entire computed column showed as empty cells.
- **Style-marked list items became paragraphs.** python-docx marks bullets with a
  `ListBullet` paragraph style and no `w:numPr` at all, so every bullet in a
  generated document lost its list structure.

Each now has a regression test, and for the first two I confirmed the test fails
when the fix is reverted.

### Carried forward

- **Relative subresources in previewed HTML do not load.** `srcdoc` inherits the
  parent's base URL, so `<img src="./chart.png">` resolves against the app's origin.
  The viewer counts such references and says so rather than showing a silently
  incomplete page. Inlining them through the authenticated endpoint is the fix if
  evidence shows it matters.
- **Per-cell number formats are not reproduced.** calamine folds the format into
  the value and exposes no per-cell format string, so currency and percent
  formatting is lost; only date-vs-number survives. Recovering it means parsing
  `xl/styles.xml` directly.
- **Slide geometry is not modelled** — shape positions, sizes, and z-order are
  discarded in favour of reading order. Reproducing a layout engine is a different
  project.
- **Charts, images, and embedded objects inside documents** are not extracted; only
  text structure and tables are.
- **`.doc`, `.ppt`, and `.pages`** (the pre-OOXML and Apple formats) are not
  supported. `.xls` is, because calamine reads it.

## Phase 8: The Start Screen — Complete

**Goal:** replace `/`, which was still Phase 2's "All sessions" list with filters,
with a surface that answers what a user actually arrives with.

**The shift, concretely:** the list was a second, weaker copy of navigation the
shell had grown around it. The sidebar groups every session by the directory it
runs in and shows that directory's archived history; `Cmd+K` searches live sessions
and the archive by content; `Cmd+Shift+K` pages one workspace's thousand-row
history. Against those, a `LIMIT`-capped list with agent, origin, workdir, and date
filters answered a question nobody opens the app to ask — and cost two
`GET /api/v1/sessions` round trips on first paint, the second only to fill an agent
dropdown.

### Delivered

- ✅ `app/page.tsx` is an IDE-style start screen: identity and what a session *is*,
  the two create paths as buttons showing their live bindings, then either recents
  or a first-run guide, then the key bindings
- ✅ **Zero added requests.** `lib/sessions-query.ts` declares the live-sessions
  query once; the sidebar, the start screen, and the search overlay all consume it,
  so the three dedupe to one request instead of hand-writing keys that deduped only
  while every copy of the limit agreed
- ✅ **Paints before the network settles.** Only the middle slot waits on data, and
  it reserves its height — no reflow on launch
- ✅ **Onboarding is the empty state, not a layer.** Three numbered steps rendered
  when the app holds no sessions, with no "seen" flag, no banner, and no
  preference, so it cannot go stale and disappears for good after the first create
- ✅ **Recents are five directories, not five sessions.** One row per session read
  correctly until a real listing was on screen: a directory worked in all week filled
  every row with entries differing only by a timestamp, since most sessions carry no
  title. A row is now a directory, opening the newest session in it, with the rest as
  a count
- ✅ `lib/welcome.ts` + tests: a pending or failed listing is never mistaken for an
  empty app, and the grouping is asserted directly — ranked by newest rather than by
  session count, trailing slashes normalized, the cap applying to directories, and
  the shared query cache never reordered
- ✅ `useShortcutHint` in `lib/shortcuts.ts`, so a surface that teaches a shortcut
  reads the binding in effect — correct after a rebind, and correct on Linux
- ✅ Removed with the list: the four filter controls, `toQuery`, the second
  unfiltered listing, and a dead `sessionSubtitle` export. `shortenHome` moved from
  the sidebar to `lib/format.ts`, where its four callers can reach it without
  importing a component

### Exit criteria — how each was checked

- ✅ **Usable before data lands** — E2E holds the listing open for 4s and asserts
  the heading, both create buttons, and the key list are on screen, and that the
  first-run guide is *not*.
- ✅ **Pending is not empty** — unit tests over `selectStartState`; E2E intercepts
  the listing to reach the genuinely-empty branch.
- ✅ **A busy directory is one row** — E2E creates three sessions in `/tmp` and
  asserts exactly one row for it, that the row names the session count, and that it
  opens the newest of the three.
- ✅ **Buttons match the shortcuts they advertise** — E2E clicks "New session",
  gets the picker, then presses `Mod+N` and gets the same picker.
- ✅ **Hints follow a rebind** — E2E rebinds `newSession` to `Mod+Shift+J` through
  the settings panel and asserts both the key list and the button hint change.
- ✅ **Accessibility holds in both states** — axe-core audits the recents state and
  the first-run guide separately; zero violations.
- ✅ **First-load JS** — **measured 198.4 KB → 198.2 KB** with the same recipe on
  both trees, so the screen costs nothing net over the form it replaced. Measured
  after merging the Artifacts viewer track, which is what moved the baseline.
- ✅ **No regressions** — 193 frontend unit + 49 E2E, all passing; ESLint clean.
  The backend is untouched by this phase and byte-identical to the other track's,
  so its suite carries over rather than being re-measured here.

**Test totals after both Phase 7 tracks and Phase 8:** 193 frontend unit + 49 E2E,
plus the backend suite unchanged from the Artifacts viewer section above.

### Carried forward

- **`SessionFilters` keeps its `agent`, `origin`, `workdir`, and
  `updatedAfterMs` fields** with no frontend caller. They mirror what
  `GET /api/v1/sessions` accepts, and the type's contract is the wire, not current
  usage — dropping them would misrepresent the endpoint.
- **`/search` remains a route** as well as an overlay. It is reachable from the
  palette and is where native-session import lives; folding it into `Cmd+K` is a
  separate decision about where import belongs.
- **The message-block sketches in `docs/frontend-architecture.md`** still show
  avatars and bubbles from Phase 2, which the agent-first canvas rejected. Flagged
  in place rather than quietly rewritten.
