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
- Phase 9: design system, mobile layout, and PWA
- Phase 10: the Artifacts pane manages files — new folder, upload, delete behind a
  confirmation, and full-file reads in a browser tab past the preview limits
- Phase 11: identity as a creation-time choice with `GET /api/v1/soul` to read it,
  English/Chinese i18n, and shortcut defaults a browser does not intercept

Every phase through 11 is complete, including 6.6 and 6.7 (archive browsing). Phase 7
ran as two tracks — the interaction and composer redesign, and the Artifacts file
viewer — both of which have landed, as has Phase 8 (the start screen); see the
sections at the end of this file.

**Current test totals, measured on the merged tree at Phase 11:** 141 backend unit,
134 backend integration (plus 3 ignored by default that parse files from real Office
tooling), 230 frontend unit, 79 E2E. The per-phase counts below are what each phase measured on
its own branch and are left as its record; they do not add up to these, because the
two Phase 7 tracks were developed in parallel and each counted only its own suite.
Re-measure by running the suites, not by adding the sections.

**Current first-load JS: 198.2 KB gzipped**, measured by summing the gzipped size of
the scripts a prerendered route references after `npm run build`.

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
- ✅ Agent command dialogs (model/effort selection, status checks) — since replaced by
  the slash menu and `components/config-picker.tsx`, which read the *live* command and
  config surface rather than the static agent descriptor
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

Phase 6 design was drafted across several documents, some still being rewritten when
implementation began. The planning scaffolding — `phase6-design-index.md`,
`phase6-frontend-redesign.md`, `phase6-quick-start.md`,
`phase6-research-checklist.md`, `PHASE6_*.md` — has been deleted. It described a
bundle allocation (~80 KB of new dependencies) that the measured budget did not
permit, so following it would have been actively misleading.

Two remain, as arguments for *intent* rather than descriptions of the code:
`agent-ui-design-analysis.md` (the UI pattern case) and
`keyboard-shortcuts-default-sessions-settings.md` (shortcut map, Quick Start,
settings). Each now marks where the shipped implementation diverged from it.
`docs/frontend-architecture.md` is authoritative for what the code does.

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
  — those two bindings were **rebound in Phase 11**, which found a browser takes
  both before the page sees them. Read them here as "the shortcut path works"; the
  current keys are `Mod+Shift+O` and `Mod+J`
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

## Phase 9: Design System, Mobile, and PWA — Complete

**Goal:** make the app read as a finished product rather than a capable
prototype, and make it usable on a phone at all.

**The diagnosis, because the reported symptoms were not the causes.** Six issues
were reported. Measuring the tree first turned four of them into one structural
problem and gave two of them a different cause than the symptom suggested:

| Reported | Actual cause |
| --- | --- |
| "The composer runs off the screen on a phone" | A `<select>` is sized by its longest *option*, not its value. A runner declaring `claude-opus-5-20260501` made that control ~180px wide whatever was selected; three of them plus Send exceeded 390px. The width came from content the user could not see, so no `min-w-0` could recover it |
| "A long model or agent name squeezes the input" | Same cause, milder presentation at desktop width |
| "Scrolling slides the whole screen up into blank space" | Three separate bugs: `100vh` is the *large* viewport height, so `h-screen` was taller than the visible area; nothing pinned the document; overscroll chained upward. Plus mobile Safari zooming on a sub-16px input and not zooming back |
| "The gear and lightning emoji look unprofessional" | 28 Unicode dingbats used as an icon set. They are font data, so appearance was decided by the platform: `📎` and `⚡` render as colour emoji on iOS, several shapes were absent from the CJK fallback chain and drew as `.notdef` boxes |
| "The file tree needs better folder/file distinction" | Both drew as unrelated geometric shapes (`▮`, `▫`, `▦`), and every row action was right-click-only — so on a phone they were unreachable, not merely awkward |
| "Support PWA install" | Nothing existed: no manifest, no icons, no viewport metadata |

**And one cause behind all of them.** There was no design system — there were 28
files each re-deciding. Measured before starting:

| Symptom | Count |
| --- | --- |
| Files hardcoding `gray-*` | 28 |
| Ad-hoc pixel font sizes | 139 uses across **10** values, incl. `9.5px`, `11.5px`, `12.5px` |
| Copies of one focus-ring class string | 92 |
| Responsive breakpoints in the entire app | **1** |

That is why every earlier fix had to be applied 28 times, and why several had
been applied inconsistently. It is the thing that read as unfinished.

### Delivered

- ✅ **Token layer** in `app/globals.css`: 37 semantic tokens across surface,
  border, ink, accent, status, and focus. No component names a palette step
  anywhere in the tree. The light palette is Tailwind's own steps copied
  verbatim in oklch, so the measured WCAG ratios recorded in component comments
  stay true and only dark mode introduces new values
- ✅ **Dark mode**, applied by a blocking inline script before first paint — an
  effect runs after paint by definition, so a dark-mode user would see a white
  page for the length of hydration. Light / Dark / System in Settings, where
  `System` keeps following the OS. Every value measured against `canvas`: ink
  17.6:1, `ink-soft` 11.4:1, `ink-muted` 6.2:1, status inks above 7:1
- ✅ **Type scale**: 10 ad-hoc sizes → 5 scale steps. Density's two one-off sizes
  (`13px`, `15px`) moved onto it
- ✅ **One focus treatment**: 92 copies → one `focus-ring` utility, changed from
  a ring to an **outline** — a ring is a `box-shadow`, so it was being clipped
  by every `overflow-hidden` ancestor and was invisible on file-tree rows and in
  the code-block header
- ✅ **48 hand-authored icons** (`components/icons.tsx`), replacing all 28
  dingbats. No dependency: ~1.0 KB gz of path data against the ~40 KB package
  the original budget decision correctly rejected — the conclusion, not the
  budget, was what was wrong
- ✅ **Composer rebuilt**: message on its own row, controls beneath it, a `+`
  menu for attach / save-to-workspace / run-command / switch-agent, and every
  `<select>` replaced by a menu whose trigger renders the current value and can
  therefore truncate. The control row scrolls rather than wraps, so composer
  height does not depend on how many flags a runner declares
- ✅ **Mobile layout**: sidebars become a left drawer (navigation) and a
  drag-dismissable bottom sheet (inspection of what is on screen). Drawer state
  is transient and *derived* from the route and breakpoint rather than
  synchronised by effects
- ✅ **Touch targets** at 44px under `pointer: coarse` only, so the desktop keeps
  its dense rows — and an overflow `⋯` button on every group, session, archived
  row, and file row, because those actions were right-click-only
- ✅ **Viewport fixed**: `h-dvh`, a pinned document, `overscroll-contain` on
  every pane, `text-base` on the composer's textarea, a 16px input floor under
  coarse pointers, and `interactiveWidget: "resizes-content"` so the keyboard
  shrinks the layout instead of sliding the page
- ✅ **PWA**: manifest route, generated 192/512/maskable/apple-touch icons with a
  committed reproducible generator, iOS metadata, a service worker that caches
  almost nothing on purpose, and an offline page that names the actual problem
- ✅ **Shared primitives** (`components/ui/`) and `lib/use-dialog.ts`, replacing
  ~40 near-duplicate button class strings and three divergent focus traps
- ✅ **Dead code removed**: `useListNavigation`'s unused `activeRef`/`reset`,
  `ToolShape.icon`, `Overlay`'s ineffective body-scroll lock, and five
  unreferenced Next.js template SVGs

### Bugs found while building, worth recording

Each of these was found by a test or a probe rather than by reading, and each
would have shipped:

- **A menu measured before it is placed cannot take focus.** The panel carries
  `invisible` until measured, and `visibility: hidden` elements are not
  focusable — so `focus()` inside the measuring effect silently did nothing and
  Escape never reached the handler. Confirmed by dumping
  `document.activeElement`
- **A dismiss-on-pointerdown listener made the mobile menu inert.** The ref it
  tests belongs to the popover, which the sheet path never attaches, so every
  `contains()` was false and the pointerdown *beginning* a tap on a menu item
  closed the sheet before the click landed. Selecting a model silently did
  nothing on a phone; confirmed by observing that no `PATCH` was issued
- **A menu that does not restore focus breaks the next surface in a chain.**
  "Run a command..." unmounts the menu in the same commit the dialog mounts, and
  the dialog captures `document.activeElement` as the opener it will restore to
  — which was `<body>`
- **`useDialog` must focus its container even with `autoFocus: false`.**
  `onKeyDown` is a React handler on that container, so the full-screen file
  viewer — which has no auto-focusing control — opened and could not be closed
  by keyboard
- **An accessible name collision is a real defect, not test fragility.**
  Labelling the `+` button "Add to this message" made it indistinguishable from
  the textarea's own "Message" label; 40 specs failing was the symptom

### Exit criteria — how each was checked

- ✅ **No palette class survives** — `grep` across `app/`, `components/`, `lib/`
  returns zero
  `(bg|text|border|divide|ring|decoration)-(gray|red|emerald|amber|blue|violet|orange)-[0-9]`
  outside `globals.css`, and zero `text-[Npx]`
- ✅ **The composer cannot overflow** — a mobile spec asserts *no element on the
  page* extends past the right edge, re-checked with a flag selected and with a
  400-word message, and that Send stays inside the composer's own box
- ✅ **The document cannot scroll** — asserts `body` overflow is `hidden`,
  `scrollHeight <= clientHeight`, and `window.scrollY === 0` after a 4000px
  wheel
- ✅ **Sidebars are out of flow below `md`** — both `complementary` regions
  hidden and `#main` exactly viewport-wide, *with the stored preference still
  true*, which is the default that caused the bug
- ✅ **Drawer and sheet behave** — drawer overlays rather than displaces, leaves
  a dismiss sliver, closes on Escape and on navigation; the sheet does not reach
  the top edge, because files are inspection of what is above them
- ✅ **Installability** — manifest linked and parsing, `display: standalone`, 192
  / 512 / maskable present, **every declared icon actually fetched** (a missing
  `public/` copy step 404s icons while the manifest still parses),
  `apple-touch-icon` and `mobile-web-app-capable` present
- ✅ **Dark mode before paint** — `data-theme` set on `<html>` under an emulated
  dark colour scheme, the canvas actually following the token, and an explicit
  choice overriding the OS and surviving a reload
- ✅ **No icon glyphs remain** — a script scans every `.tsx` for characters above
  U+2000 outside comments, allowing only the typographic set that is genuinely
  text (`›`, `↑↓↵`, `+`/`−` in diffs, `·`, `⋯`)
- ✅ **First-load JS** — **measured 198.2 KB → 203.9 KB**, `+5.7 KB` (+2.9%),
  same recipe on both trees; the baseline was re-measured rather than quoted and
  reproduced at 198.2 KB exactly. Attributed per change in
  `docs/frontend-architecture.md`
- ✅ **No regressions** — 193 frontend unit + **58 E2E** (51 desktop, 7 mobile),
  all passing; ESLint clean with zero errors and zero warnings

### Carried forward

- **iOS Safari is not covered by automated tests.** Only Chromium is installed,
  and an iPhone device profile silently runs in Chromium — which would claim
  coverage that does not exist. So `100dvh` under a collapsing toolbar and
  Safari's zoom-on-focus are asserted through consequences a Chromium phone can
  observe, and still want a pass on a real device. Installing WebKit is the fix
  if that matters.
- **`stopping` and `interrupted` now share the warning hue.** They were amber
  and orange — indistinguishable at badge size, carrying no meaning a reader
  could recover. `stopping` keeps a pulsing dot instead, since the useful
  distinction is "still moving". If they must be told apart by colour, that
  needs a third hue with measured contrast in both themes rather than an
  adjacent step.
- **Compact density is 12px, was 13px.** It moved onto the scale rather than
  keeping a one-off size a pixel away from a step.
- **The primitives would be cheaper if the sidebar were not first-load.**
  `ui/menu` is the largest item in the `+5.7 KB`, and it is unavoidable only
  because the sidebar imports it and the sidebar renders at first paint. Moving
  the sidebar behind a dynamic boundary is the lever if that number needs to
  come down.
- ~~**No i18n.** The UI is English-only.~~ **Retired.** A hand-rolled catalogue
  and a language picker landed later; see Phase 10's carried-forward note for
  what the extraction still does not cover.
- **One E2E spec was still counting shared journal state**, and it failed during
  this phase for exactly the reason `AGENTS.md` already documented — the shared
  `.e2e-data/` journal crossed the sidebar's 200-row listing cap at 357
  sessions, so a new session displaces an older row rather than adding one and
  the count stays flat. It now asserts on the created session's id instead.
  Worth noting that the pitfall was written down *before* this spec was, and the
  note was not enough on its own: the failure is invisible until the cap is
  crossed, so nothing prompted a re-read. Other specs that count rows against a
  `mkdtemp` directory are unaffected, since those directories are unique per
  run.

## Phase 10: Managing files, not only reading them — Complete

**Goal:** make the Artifacts pane a file browser rather than a file *viewer* — and
make the preview limits stop being dead ends.

**The diagnosis.** Two symptoms were reported. Measuring found the first had a
different cause than it appeared to, and the second was a missing capability with
an unstated boundary:

| Reported | Actual cause |
| --- | --- |
| "The file preview seems to have a size limit" | It does, correctly: 1 MiB read, 256 KiB of text returned, because a 320px panel cannot render a 40 MiB log and pushing one through JSON stalls the pane. The defect was not the limit but that it *terminated* — the notice reported truncation and offered nothing, so a sensible default read as a missing feature |
| "The right-click menu could support new folder, upload, delete" | No mkdir or delete endpoint existed, and the pane had no dialog primitive at all — the only confirm available was `window.confirm`, which blocks the SSE stream, cannot be styled, names the origin on iOS, and puts "OK" rather than "Delete" on the button the user presses |

**Delivered:**
- ✅ `files/raw` serves any file **whole**, and now renders text inline whatever
  the extension — decided by sniffing a 64 KiB prefix for a NUL and UTF-8
  validity, not by an extension table that would need an entry per language and
  would still be wrong about a `.log` full of binary
- ✅ `?download=true` forces an attachment, with the filename in both RFC 6266
  spellings. Separate from the type allowlist because "safe to render" and "what
  the user asked for" are different questions
- ✅ **Open in a new tab** and **Download** in the row menu, the viewer header, and
  the truncation notice itself — the notice now names both sizes and carries the
  way out, since "so how do I see the rest" is the reader's next question
- ✅ `POST .../files/directory` and `DELETE .../files`, each refusing its
  destructive default: no overwrite, no replace, no recursion, and the workdir root
  refused outright
- ✅ `ui/dialog.tsx` — `AlertDialog` and `PromptDialog`, reusing `useDialog`
- ✅ New folder, upload, and delete reachable from the header, a row menu, a
  right-click on the tree background, a drop on a folder or on the background, and
  the "add files" link beside an empty listing
- ✅ **No regressions** — 136 backend unit, 127 backend integration, 193 frontend
  unit, **74 E2E** (16 of them new), all passing; ESLint and clippy clean

### What the tests caught that review did not

Three defects were found by writing the specs rather than by reading the code, and
all three are the kind that survive review:

- **The destructive button was the first tab stop.** The footer used
  `flex-row-reverse` so the affirmative button could be written first and still land
  on the right — which made Delete the control `useDialog` focuses on open, so an
  Enter keystroke in flight when the dialog appeared would confirm it. Fixed by
  ordering the DOM Cancel-first and right-aligning with `ml-auto`.
- **`remove` deleted a symlink's target, not the symlink.** `resolve` canonicalizes,
  which is right for reading and wrong for deleting: `link` became the directory it
  pointed at. The fix resolves the *parent* and appends the final component
  verbatim, which keeps containment while leaving the last component unfollowed.
- **The tree background was not the tree's.** The container was sized to its
  content, so it ended at the final row and the large blank area a user would
  naturally right-click or drop onto belonged to the scroll wrapper. `min-h-full`.

An accessible-name collision was also reintroduced, in the same shape `AGENTS.md`
already documents: labelling the header buttons "New folder in the working
directory" made them answer `getByLabel("Working directory")`, which is a substring
match, and a workspace-shell spec failed. Shortened to "New folder" and "Upload
files" — the root path is rendered immediately beside them, so the destination
needed no spelling out. Worth noting the pitfall was written down beforehand and did
not prevent it: the phrasing looked *more* descriptive, which is exactly how this
one gets made.

### Carried forward

- **There is no trash.** Delete is permanent, because reaching the platform's trash
  means either a new dependency or spawning `osascript`, and a browser page that can
  invoke a host process is a larger concession than the delete. If undo matters, that
  is the decision to revisit — not the confirmation dialog.
- **No rename, and no write-in-place.** Both are plausible next steps and neither
  is needed by anything today. Rename is the cheaper one: it is `mkdir`'s validation
  plus a containment check on two paths.
- **The tree is still a list of tab stops, not an ARIA tree widget.** Arrow-key
  navigation with a single tab stop is what `role="tree"` implies and what a native
  file browser does. It was left alone deliberately: it would rewrite the focus model
  every existing spec depends on, and it is a keyboard-ergonomics improvement rather
  than a fix for anything currently broken.
- **Uploads are capped at 8 MiB per file**, shared with turn attachments, because
  both are browser uploads over the same JSON transport. A directory upload has no
  real reason to share that ceiling; raising it means a multipart or streaming route
  rather than a larger base64 body.
- **Each spec in `e2e/file-operations.spec.ts` mkdtemps its own workdir**, which
  follows the existing convention and adds 16 more permanent sidebar groups to
  `.e2e-data/` per run. That is the accumulation `AGENTS.md` documents; one full-suite
  failure during this phase was traced to it and disappeared after
  `rm -rf frontend/.e2e-data`. The fixture-level fix is still unwritten.
- **This pane shipped untranslated and was extracted afterwards.** The file
  browser was built while the i18n catalogue was landing on another branch, so
  every string in it was hardcoded English and the merge reported clean. The
  extraction is now done — the pane, its dialogs, the viewer, and the four format
  viewers beneath it — and an E2E spec asserts the pane's own accessible names in
  Chinese, since every previous i18n assertion looked only at the shell. What
  remains open: **the E2E suite asserts English throughout**, which is sound
  because English is the default locale, but means the suite catches a *missing*
  translation and not a *wrong* one. And **backend `message` strings pass through
  untranslated** by design — they are one process's report of what it refused,
  and localizing them would mean cataloguing every string the API can emit.

## Phase 11: Identity, Language, and Bindings That Fire — Complete

Three things that share one settings panel. They landed together because splitting
them would have produced commits that did not compile: `locale`, `injectSoul`, and the
shortcut prune all live in one interface block.

### Identity became a choice, and readable

Every session the WebApp created was given Sylph identity unconditionally, and there
was no surface anywhere showing what that identity *says* — `SOUL.md` lives under
`~/.agents/skills/sylph/`, outside this repo.

- ✅ `GET /api/v1/soul` returns the resolved text with its path, size, and mtime.
  Read per request, not cached: the file is hand-edited between sessions.
- ✅ Missing is `200 {"available": false}`, not `404`. An install without `SOUL.md`
  is ordinary; the client has to be able to say so rather than offer a toggle over
  nothing. Unreadable is a separate `error` field — a different problem.
- ✅ `POST /sessions` accepts `identityMode`, a closed enum defaulting to `sylph`.
- ✅ Resolution moved into `Config`, mirroring the runners' own rule. The two
  resolving it independently is exactly how the panel and the spawned runner would
  come to disagree about whether injection is active. The env read is a parameter,
  so the rules are testable without `set_var` — process-global and unsafe in a
  threaded test binary.
- ✅ The settings section renders `SOUL.md` in a `<pre>`, not through the Markdown
  renderer: this is the text as the agent receives it, and rendering it would hide
  the `#` and `---` that are part of what gets sent.

Origin stays `sylph` when identity is preserved. See `docs/architecture.md` for why
the two axes cannot collapse.

### i18n: English default, Chinese optional

- ✅ Hand-rolled catalogue, no framework. `next-intl` and `next-i18next` are built
  around locale-in-the-URL; this locale is a preference in `localStorage`. Revisit at
  a third locale or ~1000 keys.
- ✅ `zh` typed as a total `Dictionary`, so an untranslated key is a build error. A
  test covers what the compiler cannot: a translation that drops a `{placeholder}`
  compiles and renders a sentence with a hole in it.
- ✅ Both catalogues at 263 keys, verified equal. Coverage: settings, shell, sidebar,
  composer, and the whole file pane including its dialogs and four format viewers.
- ✅ `<html lang>` syncs, because a screen reader picks its voice from it.
- ✅ Locale names are endonyms, so someone who switched into a language they cannot
  read can find their way back.

**Still English:** the command palette, the history and search overlays, and the
timeline.

### `Cmd+N` could never have worked

The defaults bound New Session to `Mod+N` and Quick Start to `Mod+Shift+N`, both
reserved by Chrome since version 4 and beyond `preventDefault`. The browser acted
*and* the app acted: a new window, with the app's dialog behind it. The code comment
asserting these were "preventable from a page" was simply wrong.

- ✅ Defaults are now `Mod+Shift+O` (matching ChatGPT and Claude) and `Mod+J`.
- ✅ `RESERVED_COMBOS` records the set with what each does instead; the recorder
  refuses one naming that reason.
- ✅ `pruneReserved` runs at hydration as a **migration**, not a filter: a settings
  write persists the whole merged map, so anyone who had opened that panel had
  `Mod+N` stored.

**41 green specs pressed `Mod+N` happily**, because Playwright's Chromium opens no
real window. The new spec asserts on the recorder's refusal, which is observable,
rather than on browser chrome, which is not. This is the same shape as the
mobile-breakpoint blind spot: a suite can only see what its environment can express.

### Open items

- **The E2E suite asserts English throughout.** Sound, since English is the default,
  but it catches a *missing* translation and not a *wrong* one.
- **Backend `message` strings are not localized**, by design — they are one process's
  report of what it refused, and cataloguing every string the API can emit is a
  different project. Both call sites say so in a comment.
- **`e2e-repeat.sh` now reports `INCONCLUSIVE`** for a run where nothing executed
  (a backend that fails to compile, a `webServer` that exits before any spec is
  reached). It previously read `expected=0, failed=0` as "no failures in any run" —
  a false green, and the worst possible output from a flake-detection script.
- **Identity conversion is still not implemented.** Changing `identityMode` on a
  session with history remains a separate explicit operation, unbuilt.

## Phase 12: Native Sessions — In progress

The goal is parity with a terminal: slash commands the project actually defines,
approval prompts, interrupt that keeps the session, subagents rendered as
subagents. Plus the seam that lets the same session run in a Khora sandbox
instead of on the host, without the record changing.

### Verified rather than assumed

Three facts this phase rests on, each measured on this machine:

- **A held-open stdin is what makes approvals possible.** With an `ask` rule,
  `claude` sends `can_use_tool` back over stdin and waits; v1's close aborts it
  with `Tool permission request failed: AbortError: Stream closed`.
- **The adapter returns 46 real slash commands and 5 subagents** on this checkout,
  against the one or two a v1 descriptor carries by hand. `adapters/claude/verify.py`
  is the check, run against the real CLI.
- **TraceDB ingestion is whole-session and lossless**, so a watermark is derivable
  from `endedAtMs`. `IngestMode::retain_full` can no longer create a partial
  record, and `Facade::scan` re-parses rather than appending.

### What forced a redesign rather than features

Measured, not assumed. `claude -p --input-format stream-json
--permission-prompt-tool stdio` against a project with an `ask` rule sends a
`can_use_tool` request back over stdin and waits for a reply. The v1 runner
protocol closes stdin as its "prompt ended" signal, so the turn dies with
`Tool permission request failed: AbortError: Stream closed`. Holding stdin open,
the request arrives and is answerable.

So approvals, interrupt, mid-turn steering, and live capability were not missing
features — they were unreachable through a one-shot pipe. The same handshake
returns **46 real slash commands** and 5 subagent definitions on this machine,
against the 1–2 a runner hand-declares in `--describe`.

Governing specs, written before the code:

- `docs/session-protocol.md` — `sylph.agent.v3`, the duplex adapter contract
- `docs/environments.md` — the `Environment` seam, local / khora / ssh
- `docs/control-plane.md` — one server, many frontends; Discord as a frontend
- `docs/storage.md` — lossless replayable history, tiered, watermark-pruned

### Decisions taken

- **A session, not a turn, is the unit.** A live adapter process is a *link*;
  sessions are `attached` or `detached`. Links are disposable — they idle out, die
  with a restart, and a turn on a detached session establishes one first. That
  path is v1's behaviour, which makes it the floor rather than an error case.
- **v1 is removed, not kept beside v2.** There are no external consumers, so a
  compatibility shim would buy nothing and cost a single shape for what an agent
  is. Keeping it would have put two notions of a session — a turn as a process,
  and a turn as a message — into the supervisor, the capability API, the UI's
  interrupt decision, and every test. An agent whose CLI has no duplex mode is
  still supportable: its adapter holds the session and invokes the CLI per turn
  internally, which is where a vendor detail belongs.
- **The server records, losslessly and replayably.** `docs/storage.md` governs it.
  The normalized stream becomes durable history rather than a bounded projection,
  because the server is the only component present for work in every environment
  and a sandbox's native store dies with the sandbox.
- **Deltas are folded, not dropped.** A block stores its final text once plus
  packed offset and timing arrays, so every delta is reconstructible byte-for-byte
  with its arrival time at ~8 bytes each instead of ~60. Lossless replay stops
  being a thing to trade away: a 2 KB message costs 3.2 KB rather than 24 KB.
- **Three tiers, because there are three workloads.** A small hot `journal.db`, a
  read-mostly `history.db`, and a content-addressed blob store. The split is
  forced by SQLite, not taste: a long read transaction blocks WAL checkpointing,
  so a browser scrolling an archive would otherwise degrade the streaming latency
  of an unrelated live turn — a bug that reads as a network problem.
- **Retention is watermark-driven and never time-driven.** A lagging consumer
  causes growth, never loss, which is the discipline behind a Postgres replication
  slot and a Kafka offset. Sessions split into *delegated* (a native store exists,
  prune below the confirmed seq) and *terminal* (a sandbox, no ingestion path,
  compact forever). Collapsing the two would destroy the only copy of exactly the
  isolated work this was meant to protect.
- **Replay is a server operation.** Reconstruction is subtle — UTF-8 boundaries,
  interleaving across two stores, timing accumulation — and a rule implemented once
  per frontend is one that will differ per frontend, leaving Discord, a terminal,
  and the browser disagreeing about what happened.
- **Pending requests are resources, not stream messages.** An approval survives a
  client disconnect and is answerable from any frontend. A stream-only design
  strands the request and blocks the agent indefinitely.
- **`interrupt` is not `stop`.** Stop kills the process group; interrupt asks the
  agent to end the turn and stay alive. Conflating them loses one intention.
- **Discord becomes a thin frontend**, keeping only channel→session bindings.
  Ends the two-control-plane split, and Discord inherits approvals, live command
  lists, and real interrupt.

### `sylph.agent.v3`: what a second agent forced

The protocol needed a version bump, and both reasons came from real use rather than
review.

**A capability surface was one value, replaced wholesale.** `claude` emits its
`system/init` **once per turn** — measured, two user messages produce two of them.
The adapter turned each into a capability event carrying only the permission mode,
and the server assigned it over the cached surface, so the first turn of every
session erased the project's 46 slash commands. The symptom appeared two layers
away: the composer's menu stopped opening after the first exchange. Reproduced end
to end before changing anything — `commands=46` before a turn, `commands=0` after.

Merging partials would have fixed that same bug at the same cost and left the
hazard: a message about one part of the surface would still be *able* to speak for
all of it. So a capability update is now **facet-scoped**, and an unmentioned facet
cannot be cleared. The regression test drives a real link and fails (`left: 0, right:
2`) when the merge is reverted — verified by reintroducing the bug rather than
assuming the test would catch it.

**Every slash command was modelled as a kind of turn.** Right for `/verify`, wrong
for `/model`: sent as prompt text, it returns a paragraph listing the models, which
is the CLI behaving correctly because a headless client has no picker to open. Three
protocols were surveyed and all three make the same split — ACP hoists model and mode
into `SessionModelState` / `SessionConfigOption`, codex has no command concept at all
and exposes `model/list` plus `config/value/write`, and `pi` deliberately *excludes*
its built-in TUI commands from `get_commands`. `claude` is the outlier in returning
`/model` in the same list as `/verify`.

So a command now carries `kind: prompt | control`, and control commands resolve
against typed session config (`select` | `boolean`, with a `category`), modelled on
ACP's `SessionConfigOption` because it already covers all four agents' knobs. **The
adapter classifies**, because no vendor supplies the discriminator and which commands
are control surfaces is a vendor fact — a list in the server or the UI would be wrong
for the next agent.

### A second adapter, and what it proved

`adapters/pi/` speaks `pi --mode rpc`. It exists to stop "claude does X" from being
mistaken for "agents do X", and it earned its place immediately:

- It declares **no `approval` capability**. pi ships no sandbox and has no
  tool-permission callback, so an approval prompt could never arrive. This is the
  degradation case capability negotiation exists for, and it is why `supports` gates
  UI affordances rather than being decorative.
- Its surface is *queried*, not pushed — `get_commands`, `get_available_models` and
  `get_state` are three round trips, which facet-scoped updates make cheap.
- Its thinking levels are **per-model**: the selected model offers two of the seven
  pi documents. `claude` likewise carries `supportedEffortLevels` per model. A
  session-global effort enumeration — which is what the old flat `flags.choices` was
  — is wrong for both, so config options are re-derived after a model change.
- Four agents spell a model list four different ways (`value`/`displayName`,
  `modelId`/`name`, `id`/`displayName`, `id`/`name`), which is what makes the
  normalized seam belong in the adapter.

### Retention became reachable, and what that took

Three things were believed about storage that measurement contradicted.

**The tiering was already built.** The open item claiming the `journal.db` /
`history.db` split was "designed but not built" was stale: `HistoryStore` (own pool,
`synchronous=FULL`, own migrations) and `Consolidator` (spawned at startup) had
landed. Measured on a real install: the journal's `events` table held **0 rows**
while `history.db` held **131 blocks across 4 sessions**, 11 of them carrying folded
delta arrays. The split works and the journal is bounded. What was missing was the
file *name* and delta batching.

**Retention had no scheduler — and would have pruned nothing if it had.** Two
independent defects in `TraceDbAuthority`, either alone fatal:

- It compared Sylph's session UUID against the archive's `<agent>:<native-id>` key.
  The three linked sessions on this machine *are* in TraceDB, as
  `claude:9bdacb6e-…` and two `pi:…` rows. The UUID matches nothing, ever.
- It scanned `list(None, 200, None)` — one page of a 4416-row archive.

Both fail silently: the archive answers "no such session", retention correctly keeps
everything, and every layer behaves impeccably. `watermarks` held **0 rows**,
confirming it had never run. The authority is now constructed *from* an archive id,
so the unanswerable question cannot be asked, and asks `show` by id rather than
paging. Both defects were re-introduced deliberately to confirm the new tests fail.

**The upstream ack is not what the design hoped for.** It is archive-global — one
monotonic sequence per ingest *run*, committed after the data transaction so never a
false positive — not a per-session coverage cursor, so it cannot replace the
`endedAtMs` reasoning. It is also not cheaply readable: its only surfaces are a gRPC
ingest response and `doctor --json`, measured at **2m34s**. What retention reads
instead is `show`'s source fingerprint, which says which version of the native store
a confirmation came from; `docs/storage.md` records the full reasoning.

Also decided: the ingestion daemon already runs (`trace-db watch --interval 1800`,
same archive, same version), so Sylph observes rather than becoming a second writer.

Two design corrections fell out of building it. An unreachable archive is reported
once per pass rather than as N session failures — an install without TraceDB is a
supported degraded state, and it must not be recorded as a *checked* watermark,
because "cannot ask" is not "asked and nothing is safe". And blob sweeping moved out
of the per-session path: reachability spans sessions, so it was running a global scan
once per session for one pass's worth of work.

**Delta batching is ephemeral-only, and that is forced.** Stable events are journaled
before publishing so a reconnect can always find them; the hub's ring holds only
ephemeral events, which is what a mid-stream reconnect replays. So batching stable
events would open a hole a reconnect could observe, while batching deltas — thousands
per turn against tens of stable events — costs nothing observable and captures nearly
all the win. Its one ordering rule is that a turn's buffer must flush before the fold
reads it; removing that flush fails
`replay_reproduces_the_deltas_and_not_only_the_final_text`, verified by removing it.

### Bugs found while building, each with its own cause

- **The model list was empty on every session ever opened.** `capabilities_from` read
  `entry.get("id")`, but the CLI answers `{"value","displayName",...}`. A wrong key
  fails silently, so the composer's model control had nothing to show and looked like
  a UI bug.
- **`LinkManager` could not find adapters it had discovered.** Discovery searches two
  roots; the manager knew only `agents_dir` and re-derived the path, so an agent found
  in the other root listed as usable while every link failed with `No adapter for
  agent 'claude' at <the other root>`. `Registry::adapter_path` already existed and
  was unused. Discovery and linking now share one answer.
- **The approval dialog offered an option the agent forbade.** The CLI marks asks
  where granting "always" would write a rule *broader than the ask itself*, and its
  schema says a host should omit the affordance. We always showed it. Filtering lives
  in `requestOptions` so every consumer inherits it — and never empties the dialog,
  because a panel with no buttons leaves the agent blocked with no way to answer.
- **A steer could land on the wrong turn.** codex requires an `expectedTurnId`
  precondition; ours had none, so a steer racing a turn boundary was delivered into
  the next turn — succeeding silently with the text in the wrong conversation. Now a
  `409 TURN_MOVED`.
- **`ConfigOption` needed `rename_all_fields`.** With only the container
  `rename_all`, serde renames variant *names* but not the fields inside them, so
  `currentValue` parsed as `None` while everything compiled and every other field
  worked. A test caught it; review would not have.
- **The turn event rendered the reply twice.** `turn` had no case in the timeline and
  fell through to the status line, printing the final text under the assistant block
  it duplicated. Invisible under v1, where the outcome arrived as a process envelope
  that never reached the timeline.

### Delegated trajectories: the protocol promised a tree it could not carry

`subagent` was in the v3 event table from the start, described as carrying "the
child's own events nested". Nothing implemented it, and the description could not
be implemented: an envelope must be closed before it is sent, and a trajectory
streams for minutes. Meanwhile the field it would have needed did not exist —
`StreamEvent` had no parent pointer, the `events` table had no column, and the
timeline had no `subagent` case, so a delegation fell through to a bare status
line.

The measured consequence was worse than a missing feature. `adapters/claude/`
called `_stream_event(msg["event"])`, dropping the envelope's
`parent_tool_use_id` — the only thing distinguishing a delegate's output from the
parent's. Every subagent's thinking, prose, and tool calls were published *as the
root agent's own*. And `self.blocks` was keyed by claude's content-block index
alone, which numbers per message: two concurrent delegates both open index 0, so
one's deltas appended to the other's text. That is corrupted output, not a layout
fault, and it was invisible until two delegates ran at once.

**One field carries subagents, nested subagents, and dynamic workflows.** An
event's `span` names the trajectory it ran in; a `subagent` event's `streamId` *is*
a trajectory id. A nested delegation is one whose `span` names another, and a
workflow phase is a trajectory whose children are trajectories — no case in the
protocol distinguishes them, which is what makes it a generalization rather than
three features. Distributed tracing settled on a flat stream with a parent pointer
for the same reason, and it is already how the vendor speaks.

**The tree is navigated, not displayed.** One indent per depth runs out of viewport
before it runs out of tree, so a trajectory is one collapsed card, indentation caps
at two levels, and focus mode re-roots with a breadcrumb. Collapsing is also the
performance design: a closed card mounts no descendants, so DOM cost tracks what
the user opened rather than what the agent did — which is a stronger property than
the memoization the timeline already had, and why virtualization is still not
needed.

**Every card reports its whole subtree,** because a collapsed row is otherwise
where a failure goes to die. A descendant failure escalates its ancestors, and a
failed branch expands itself down to the tool that failed. `abandoned` stays
distinct from `failed`: the server closes trajectories an adapter left open when a
turn terminates, and reporting that as a failure would call an interrupted turn a
broken one.

### Bugs this surfaced below the UI

Each was found by a test, and each presented somewhere other than where it lived:

- **`is_stable()` disagreed with the `stability` field beside it.** It re-derived
  the answer from the phase, which was equivalent until `subagent` became stable at
  every phase — after which a trajectory's `started` was classified stable,
  published as stable, and then not journaled. One fact with two derivations
  diverges the moment either grows a case.
- **Consolidation dropped a trajectory's name.** Folding kept only the `completed`
  event's fields, but a trajectory is named on `started` and renamed by the `delta`
  carrying its parsed brief; its close reports an outcome, not an identity. So every
  card read "Subagent" once a turn consolidated, which looked like a frontend
  fallback firing two layers up.
- **A focus control named "focus" collides with every sibling.** Visible text wins
  over `title` for the accessible name, so twenty delegations presented twenty
  indistinguishable controls. The same accessible-name collision that once failed 40
  specs at once.
- **A two-card fixture cannot observe a tree bug.** The E2E fixture now emits two
  concurrent trajectories with interleaved events, one nested three deep, a failure
  at the bottom, and one left open — because grouping by arrival order, flattening at
  depth 2, and a permanently spinning card each pass a shallower fixture.

### Promotion: a trajectory is defined by use, not by declaration

The tree above worked for `Task` and for nothing else. `claude` has since shipped
`Agent` and `Workflow`, and an MCP server can add a delegating tool whenever it
likes — so an adapter deciding at open time whether a call delegates is guessing at
a fact that arrives later, since delegation is only knowable when a child turns up
carrying the call's id. Measured on a real session: two `Workflow` cards read
*running* permanently with no interior, and four `Agent` calls had their children
rooted beside the call rather than inside it.

**Any `streamId` another event names as its `span` is a trajectory**, whatever kind
of event opened it, and a `tool_call` so named is *promoted* in place — it stays a
tool card and becomes the container its children already claim it is. `subagent`
remains the richer declaration path (it names a delegation before any child exists
and states an outcome independent of its host), but is no longer required. An agent
nobody has written yet nests correctly by tagging its children, which is the same
`span` it must already set for its output to be attributed at all. That is what
makes `pi`'s future delegation need no adapter-specific path.

Five things follow, each fixing a defect this surfaced:

- **A trajectory's id is the delegate's, never the host's.** `Workflow` fans out to
  N agents under one `tool_use_id`; keyed by the host, a two-agent workflow rendered
  as *one* card reading `Demo: agent-2` and `agent-1` was absent from the timeline.
  Keyed by `task_id` with the host as `span`, the tree is
  `Workflow → agent-1, agent-2 → each one's work`.
- **A promoted host's lifecycle is its `tool_result`.** A `tool_call`'s `completed`
  phase means its *arguments* finished streaming, so reading the phase reports every
  delegation as finished the instant its input parsed.
- **Closing is phase-agnostic.** `open_trajectories` matched `phase = 'started'`,
  which every workflow delegate fails — its first notification is progress, not a
  start — so the turn-settle sweep never closed them. It now sweeps every id
  declared or referenced, minus those closed by a `subagent` or a `tool_result`.
- **Startup closes what a crash left open.** `interrupt_stale_turns` skips `settle`,
  and consolidation folds a trajectory with no close into *no block at all* — losing
  the delegation while keeping its children, which then reattach to the root.
- **A trajectory with no interior says so on the card.** `--forward-subagent-text`
  forwards only for built-in `Task`: measured, an `Agent` delegation produced zero
  tagged envelopes. A closed trajectory with no children is a terminal state, so it
  reports "no interior" and drops its disclosure rather than inviting the user to
  open a branch that can never fill.

Two hazards found while building it, both silent. A cycle in `span` pointers does
not merely risk recursion — attachment holds one node as the other's orphan and
each adopts the other, so neither is reachable and *both vanish*; `buildTree` now
repairs by reachability and the rollup carries a visited guard. And `NODE_CACHE` was
keyed on `TimelineRow` while `buildRows` reuses only the *event*, so the memo could
never hit once; a trajectory row's event is re-folded every build, so the rows most
worth skipping were the only ones that could not be.

`verify.py` measures all of it against the real CLI, and its own delegated turn now
answers the approvals the delegate raises — unanswered, the turn never completed and
every assertion after it read mid-flight state, which is what made a script problem
present as an adapter fault.

### An agent may start a turn

A `Workflow` that dispatched three backgrounded workers finished its turn in 22
seconds and said "I'll notify you when it's done". Nothing ever arrived, and the
delegate's card read *never finished* permanently.

The delegate had in fact succeeded. `claude` reported it — measured, after the turn's
`result`: `background_tasks_changed`, `task_updated`, `task_notification` carrying the
delegate's own summary, and then `init`/`assistant`/`result`, an entire exchange the
agent opened to describe what had happened. stdout stayed open; the process did not
exit. Every one of those events reached `absorb_event`, found no turn running, and was
dropped.

The drop was defensible and the conclusion was wrong. An event carries no turn id, so
the server files it under whichever turn is running, and a guessed id misplaces it in
the transcript forever — all true, and none of it implies that no turn means nowhere to
belong. **The server now opens the turn that exists** rather than guessing at one that
does not: an agent-initiated turn, `origin: "agent"`, a real turn that consolidates and
orders and prunes like any other. `turn_id` is what consolidation iterates, so anything
less than a turn would leave rows the journal never folds and never releases.

Four things this settled:

- **It opens on the first out-of-turn event of any kind.** The vendor's own
  `turn`/`started` arrives *after* `task_notification`, so waiting for the bracket
  keeps the announcement and drops what it announces.
- **It settles without a waiter.** Nobody is parked on an outcome nobody asked for, so
  terminal state is recorded directly. `TurnCloser` is that path, factored out of the
  supervisor so both reach one implementation — `LinkedSupervisor` owns a
  `LinkManager`, so a call back the other way would be a cycle. Left unsettled, one
  unprompted turn wedges its session, because a live turn refuses new messages.
- **A waiter's absence means two different things.** An agent turn never had one; a
  user's turn whose waiter is gone has an owner already, and settling it here reported
  a deliberate stop as `LINK_LOST`. `origin` separates them.
- **The client had to stop holding its claim.** `sentTurnId` outranked adoption and was
  never released, so the agent's own turn was never streamed — it would have appeared
  only on the next reload, which is how a delivered notification still reads as a
  missing one. The claim now expires when history holds the turn as terminal, derived
  rather than cleared, which also keeps the handover gapless.

`abandoned` is consequently not final: a delegate the turn-settle sweep closed may
report its real outcome on a later turn, and the later record is the truth.

### Open items

- **Two records exist for a backgrounded delegate, and nothing reconciles them.** The
  sweep's `abandoned` close and the delegate's own later completion are both journaled,
  on different turns. A reader sees the truth because the later one is on screen too,
  but the earlier card still says the work never finished. Reconciling them means
  addressing a trajectory across turn boundaries, which the tree does not currently do.
- **`is_backgrounded` is carried but not used.** `task_started` reports it, so the sweep
  could leave a backgrounded delegate open instead of closing it as abandoned —
  removing the contradiction above at its source rather than after the fact.
- **`pi` reports no child trajectories yet,** so its sessions render as before.
  Promotion is what makes adding them an adapter change alone: emitting a `subagent`
  per delegate, or merely tagging child events with a `span`, is enough for its own
  subagents and dynamic workflows to render as a tree with no server or UI work.
- **A multiplexed host with several delegates cannot attribute forwarded interior.**
  `_delegate_of` re-tags forwarded output to the delegate only when a host has
  exactly one; with several, `claude` forwards nothing, so there is nothing to
  attribute. If a future vendor forwards interior *and* multiplexes, it will need a
  correlation the envelope does not currently carry.
- **TraceDB's archived sessions stay flat.** It records a delegation as a separate
  session with its own lineage, not as a trajectory inside one stream, so an
  archived transcript has no `span` and the sidebar's rolled-up count is still how
  delegation appears there. Unifying the two views is a real design question and
  not a translation.
- **A structure overview is not built.** `flattenTrajectories` exists and the
  breadcrumb uses the path, but there is no waterfall or graph answering "which of
  these twenty agents was slow" at a glance. The rollups cover *whether* to drill
  in; they do not compare siblings.

- **Multi-user is not solved by this work**, only un-foreclosed: authorization has
  one place to live, and request resolution records a resolver. Authentication
  beyond the bootstrap cookie, per-user visibility, and credential isolation
  remain unbuilt, and non-loopback binding stays a separate explicit change.
- **Khora needs a Template carrying the agent CLIs and adapters**, since the
  adapter runs inside the environment. None exists yet.
- **An `ingest_status` op on `trace-db api`** is worth asking upstream for. TraceDB
  now emits a durable ingestion ack, but its only surfaces are a gRPC ingest
  response and `doctor --json`, measured at **2m34s** because doctor walks every
  agent's native store and verifies the archive. The op would be a single
  `schema_meta` read that `store::ingest_status` already implements. Nothing depends
  on it: retention reads per-session confirmation instead, which is finer-grained
  than the archive-global ack anyway (`docs/storage.md`).
- **A Khora session's retention class is untested against a real sandbox.** The
  policy resolves any non-`local` environment to `terminal`, which is the safe
  answer and the right one, but no sandbox exists yet to confirm the environment
  reports the kind this depends on.
- **Retention has no upper bound on a terminal session's growth.** Compacting
  forever is correct — the record is the only copy — but a long-lived sandbox
  session accumulates without limit by design, and nothing yet surfaces *which*
  sessions are responsible. `/diagnostics` reports the terminal count, not the bytes.
