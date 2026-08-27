# Frontend Architecture

Sylph Web frontend is a Next.js 16 application providing a local-first UI for
multi-agent session management. It is an **agent workspace**, not a chat client:
tool execution is the primary content, prose is connective tissue, and navigation
is keyboard-first.

> **Reading this document.** Sections below describe the shipped design. Where an
> earlier plan was not followed, the deviation is stated with its reason rather
> than quietly deleted — a spec that hides why it diverged from the code teaches
> the next reader nothing. The largest deviations are: no Zustand (React context
> plus an external store), no Radix UI, no Shiki, no `react-markdown`, and no
> virtualization. All four are bundle-budget decisions, explained in
> **Performance Optimization**.

## Reference Implementations and Research

**DeepSeek Harness is a reference, no longer the directive**

DSH source is still worth reading when a UI question has an established answer:

```
/Users/hsw/Workspace/oss/deepseek-harness/repo/apps/web/
/Users/hsw/Workspace/oss/deepseek-harness/repo/packages/web/
```

This section originally read "PRIMARY DIRECTIVE: default to DSH's patterns" and
scoped itself to Phase 2. Three of its instructions have since been overtaken by
measurement, so it is recorded as history rather than followed:

- **Not the canvas.** DSH renders message bubbles; Phase 6 rejected that shape for a
  flat tool-anchored timeline. See "Conversation canvas: agent-first, not chat-first"
  and `docs/agent-ui-design-analysis.md` for the argument.
- **Not the transport.** The `EventSource` design copied from here shipped a bug that
  dropped the entire stream. `lib/use-sse.ts` reads the body with `fetch` instead —
  reasons at "Why not EventSource" below.
- **Not the dependency choices.** DSH is measured against a different bundle; the
  no-dependency decisions here are measured against this one.

What still transfers: layout structure, streaming-text handling, and the general
lesson that reading a production implementation beats guessing.

**On researching before designing**, the rule is `AGENTS.md` § Design Decisions and it
is not repeated here. A generic five-step research workflow used to occupy this spot,
with a worked example concluding that SSE reconnection should use exponential backoff
"proven in both eventsource lib and DSH" — advice this codebase does not take, because
`lib/use-sse.ts` resumes from `after_seq` with its own reader rather than delegating
reconnection to `EventSource` at all. A section teaching a method by an example the code
contradicts teaches the wrong thing twice. The findings that method produced are in
this file, next to what they govern: **Why not EventSource**, the dependency costs in
**Technology Stack**, and the measurements in **Performance Optimization**.

## Design Philosophy

**Core principles:**
1. **Honest capability** — a session shows what can be observed, what can be driven, and how much resolution its record has
2. **Real-time first** — SSE-driven updates with optimistic rendering
3. **Local-first** — No external dependencies, works offline after initial load
4. **Accessibility** — WCAG 2.1 AA compliance, keyboard navigation, screen reader support
5. **Performance** — Keep bundles compact, optimize rendering, maintain responsiveness
6. **Architecture quality** — Balance performance, user experience, maintainability, and extensibility

**Taken from DeepSeek Harness, and what was not:**

| Borrowed | Diverged, with the reason recorded below |
| --- | --- |
| Layout: rail plus canvas, now three panes | Message bubbles → flat tool-anchored timeline |
| Streaming text handling | `EventSource` → `fetch` body reading |
| Attachment handling in the input area | Input area → one composer surface carrying agent, model, effort |
| Thinking block collapse/expand | Uniform expansion → expansion by role (edits open, shell output on failure, reads closed) |
| Error-state and loading patterns | Syntax-highlighting library → hand-rolled `code-block.tsx` |

**Sylph-specific, with no DSH equivalent:**
- Per-session agent, model, and effort flags, read from the agent's own report
- History browsing and search with lineage rollup
- Link capability facets: observe and control reported separately, each with a reason
- Fidelity indicators, so a duration is only drawn where one was recorded
- Environment picker, including whether an environment is reachable only via a relay

Lifecycle indicators (24h idle, 04:00 reset) are modelled in the backend and surfaced
nowhere in the UI yet.

## Technology Stack

**Core framework:**
- Next.js 16 (App Router)
- React 19 (with Server Components where applicable)
- TypeScript 5.7+ (strict mode)

**State management:**
- TanStack Query v5 (server state, caching)
- React context for shell state; `useSyncExternalStore` over `localStorage` for
  preferences. **No Zustand:** two contexts and one external store cover every
  case here, and a state library would add a dependency for a store that holds
  about a dozen fields.
- **Not the `EventSource` API.** SSE is read with `fetch` and a hand-written frame
  parser (`lib/use-sse.ts`, `lib/sse-parse.ts`). `EventSource` dispatches by frame
  name, so `onmessage` receives almost nothing; it also cannot carry a resume cursor
  or be unit-tested. See **Why not EventSource** below.

**UI components:**
- Tailwind CSS 4 (utility-first styling)
- **No component library.** The primitives needed — modal, menu, drawer, bottom
  sheet, disclosure, listbox — are implemented directly in `components/ui/`,
  `components/overlay.tsx`, native `<details>`, and the two picker overlays. Radix UI would have supplied them with better
  edge-case coverage, at roughly the whole remaining first-load budget. Native
  `<details>` in particular is *better* than a scripted disclosure: it gives
  keyboard operation, `aria-expanded`, and find-on-page behaviour with no script.
- Inline SVG and text glyphs for icons. **No icon package** for the handful of
  shapes in use.
- **No animation library.** Tailwind's `animate-pulse` covers the only motion in
  the UI, which is streaming and running indicators.

**Code rendering:**
- `components/markdown.tsx` — a block-then-inline parser covering the subset agent
  output uses: fenced code, inline code, headings, lists, emphasis, links,
  blockquotes, tables. **Not `react-markdown`** (~45 KB gzipped with `remark-gfm`),
  which alone would exceed the budget. Every node is a React element, so
  agent-authored text cannot inject markup and no sanitizer is needed; link
  schemes are still allowlisted, because a React-rendered `javascript:` href
  executes.
- `components/code-block.tsx` — monospace, line numbers, copy, truncation.
  **No syntax highlighting.** Shiki ships a WASM regex engine plus grammars;
  `react-syntax-highlighter` with Prism is ~35 KB. The way to add it later is a
  dynamic `import()` behind a settings toggle, so it never lands in first-load JS.
- `lib/diff.ts` — LCS line diff for rendering agent edits. **Not jsdiff** (~30 KB)
  or `diff-match-patch` (~70 KB), whose value is character-level diffing and patch
  formats that this UI does not render.
- `lib/fuzzy.ts` — subsequence matching with cluster-based ranking. **Not fuse.js**
  (~12 KB); typo tolerance is deliberately absent, because in a short visible list
  a near-miss costs one keystroke while a promoted wrong match costs trust.

**Testing:**
- Vitest (`npm test`) for pure logic: the diff, the Markdown parser, tool-shape
  interpretation, fuzzy ranking, the shortcut grammar, and timeline row building.
  These are the components where hand-rolling replaced a dependency, so they carry
  the test burden that dependency would have carried.
- Playwright (`npm run e2e`) for rendered behaviour, focus management, keyboard
  paths, and render cost.

**Development:**
- Turbopack (dev server and build)
- ESLint, invoked directly (`npx eslint .`). **Not `next lint`**, removed in Next 16 —
  it fails with a misleading `Invalid project directory` error naming a `lint`
  directory. No Prettier: there is no config for it and it is not a dependency;
  formatting is whatever the editor does.
- TypeScript strict mode. `npm run build` type-checks, so it is the TypeScript gate and
  there is no separate `tsc` step

## Application Structure

There is no `src/` directory: the app, component, and library roots sit at the
top of `frontend/`, which is what `create-next-app` produces and what the `@/`
path alias resolves against.

```
frontend/
├── app/
│   ├── layout.tsx              # Root layout: providers + AppShell
│   ├── page.tsx                # Start screen: begin, resume, learn the keys
│   ├── search/page.tsx         # Full-text search over the server's history
│   ├── sessions/[id]/page.tsx  # Conversation canvas
│   └── globals.css
│
├── components/
│   ├── app-shell.tsx           # Three-pane frame, top bar, Quick Start
│   ├── workspace-sidebar.tsx   # Left pane: sessions grouped by workdir
│   ├── artifacts-pane.tsx      # Right pane: file tree, swapping to the viewer
│   ├── file-viewer.tsx         # Right pane: one file, header and viewer choice
│   ├── file-viewer-overlay.tsx # The same viewer, full screen
│   ├── viewers/
│   │   ├── grid-view.tsx       # Spreadsheets and CSV
│   │   ├── document-view.tsx   # Word and PowerPoint
│   │   ├── html-view.tsx       # Sandboxed page rendering
│   │   └── media-view.tsx      # Images and PDF
│   ├── timeline.tsx            # The conversation canvas itself
│   ├── markdown.tsx            # Markdown parser and renderer
│   ├── code-block.tsx          # Code and command output
│   ├── diff-view.tsx           # Unified inline diff
│   ├── overlay.tsx             # Modal shell, focus trap, list navigation
│   ├── search-overlay.tsx      # Cmd+K
│   ├── command-palette.tsx     # Cmd+P
│   ├── settings-panel.tsx      # Cmd+,
│   ├── history-overlay.tsx     # Cmd+Shift+K
│   ├── workspace-picker.tsx    # Known workspaces, ranked and filterable
│   ├── directory-picker.tsx    # Server-enumerated folder chooser, filterable
│   ├── composer.tsx            # Message entry, + menu, config controls, attachments
│   ├── attachment-picker.tsx
│   ├── slash-menu.tsx          # The agent's own commands, as the user types
│   ├── config-picker.tsx       # Typed session config; what a control command opens
│   ├── approval-dialog.tsx     # Answering what an agent is blocked on
│   ├── badges.tsx              # Origin and status badges
│   ├── icons.tsx               # ~40 hand-authored SVG paths, no dependency
│   └── ui/
│       ├── button.tsx          # Button, IconButton, Kbd
│       ├── menu.tsx            # Context menu, dropdown, mobile sheet fork
│       ├── dialog.tsx          # AlertDialog, PromptDialog (not window.confirm)
│       └── sheet.tsx           # Bottom sheet and side drawer (mobile)
│
├── lib/
│   ├── api.ts                  # Typed client; mirrors the wire shape exactly
│   ├── providers.tsx           # QueryClient + AuthGate
│   ├── settings.tsx            # Preferences over localStorage
│   ├── create-session.ts       # Dialog-free session creation + default resolution
│   ├── workspace-ui.tsx        # Panes, overlays, drawers, artifact selection
│   ├── shortcuts.ts            # Combo grammar and platform resolution
│   ├── theme.ts                # Pre-paint theme bootstrap + application
│   ├── viewport.ts             # Is this a phone; the breakpoint constant
│   ├── use-dialog.ts           # Focus trap, restoration, Escape
│   ├── register-sw.tsx         # Service-worker registration (production only)
│   ├── timeline-rows.test.ts   # (tests live beside the code they cover)
│   ├── diff.ts                 # LCS line diff
│   ├── tool-shape.ts           # Interprets tool_call payloads per agent
│   ├── file-kind.ts            # Extension -> viewer family, in one table
│   ├── file-operations.tsx     # New folder, upload, delete: one owner, one dialog
│   ├── grid-format.ts          # Column labels, alignment, paging arithmetic
│   ├── sandbox-html.ts         # CSP injection for previewed HTML
│   ├── fuzzy.ts                # Subsequence ranking
│   ├── use-sse.ts              # Streaming fetch reader with after_seq resume
│   ├── sse-parse.ts            # Incremental event-stream frame decoder
│   ├── use-debounced.ts
│   ├── error-boundary.tsx
│   └── format.ts
│
├── e2e/                        # Playwright specs + mock runner fixture
├── next.config.ts              # Proxy rewrite, standalone output
├── vitest.config.ts
└── playwright.config.ts
```

**Why the shell lives in the root layout.** `AppShell` wraps `children` in
`app/layout.tsx` rather than being placed per page. A layout does not remount on
navigation, so the sidebars keep their expanded groups and scroll position when
moving between sessions; a shell mounted inside a page loses both on every route
change. The consequence is that pages render only their own scrolling content and
do not declare `<main>` — the shell owns `<main id="main">`, which the skip link
targets.

## UI Layout and Design

Moved to **`docs/ui-layout.md`**. This section was 1188 lines — 40% of this file — and
it answers a different question than the rest of it: what the interface *is*, rather
than how the frontend is built.

The dividing line, so a future addition lands in one place rather than both: this file
covers construction — state, streaming, data flow, performance, error handling,
testing. `ui-layout.md` covers the panes, their breakpoints, the surfaces a user
touches, the component inventory, and the design tokens. A memoization strategy is
construction even though it affects the timeline; a pane's collapse breakpoint is
layout even though a hook implements it.

## State Management

### Server State (TanStack Query)

Query keys follow a hierarchical structure:

```ts
// Live sessions. Declared once in `lib/sessions-query.ts` and shared by the
// sidebar, the start screen, and the search overlay, so the three mount as one
// request rather than three. Hand-written keys deduped only while every copy of
// the limit agreed, and the drift was silent.
['sessions', { limit }] → useQuery(liveSessionsQuery())

// Session detail
['sessions', id] → useQuery

// Agents
['agents'] → useQuery

// Turn detail
['turns', turnId] → useQuery
```

**Query invalidation:**
- New session created → invalidate `['sessions']`
- Turn completed → invalidate `['sessions', sessionId]`, `['turns', turnId]`
- Agent refresh → invalidate `['agents']`

**Optimistic updates:**
- Send message → immediately append to local cache
- Stop turn → immediately update status to 'stopping'
- Revert on error via `onError` rollback

### Client State

Two stores, neither of them Zustand.

**`lib/workspace-ui.tsx` — shell state, React context.** Which panes are open,
which overlay is up, which artifact the right pane shows, and the session-action
handler registration. This lives above the page tree because the shell outlives
navigation, and because a conversation event needs to ask the Artifacts pane to
open a file — a request that has to reach it from outside its own subtree.

**`lib/settings.tsx` — preferences, `useSyncExternalStore` over `localStorage`.**
Default agent and model, density, expansion defaults, shortcut bindings, pane
visibility, pinned workspaces, workspace display names.

The external-store hook rather than state-plus-effect, for three reasons:

1. Reading storage in an effect and calling `setState` cascades an extra render on
   every mount, which is what `react-hooks/set-state-in-effect` flags.
2. The server snapshot is a separate stable value, so hydration cannot mismatch —
   the failure mode of reading `localStorage` in a lazy initializer.
3. Subscribing to the `storage` event makes a change in one tab reach the others,
   which a per-component effect cannot do at all.

Stored JSON is merged over the defaults **field by field**, each checked against
its own shape. A blob written by an older build is missing keys a newer one needs,
and a hand-edited one can hold anything; spreading it wholesale would let
`density: 42` through and break rendering. Custom shortcuts layer over the
defaults rather than replacing them, so a binding added in a later release still
works for an existing user.

Preferences are client-side by design: every value is a display or input
preference, and none changes what the backend does with a session. The trade-off is
that they do not follow a user to another browser — acceptable for a loopback-only
single-user app, and not acceptable once a non-loopback deployment adds real
accounts, which is already gated behind a separate authentication change.

Pane visibility is read straight from settings rather than mirrored into local
state: the mirror needed an effect to adopt the stored value after hydration, and
could disagree with storage when a write failed.

### Internationalization

English is the source of truth and the fallback; Chinese is optional. The locale is
a settings field, so it rides the store above — no provider, no context, and
switching language re-renders exactly the components that read a translation.

```
lib/i18n/locale.ts   Locale set, tags, message lookup, format, formatParts.
                     No React, no settings import.
lib/i18n/messages.ts en — the `Dictionary` type and the source of truth
lib/i18n/zh.ts       zh — typed as a total `Dictionary`
lib/i18n/index.ts    "use client": useT(), useLocale(), re-exports
```

**The `locale.ts` / `index.ts` split is load-bearing, not cosmetic.** `lib/settings`
must know what a `Locale` is in order to store and validate one, while `useT` must
read `lib/settings` to know which locale is active. Keeping the primitives free of
both React and the store makes the dependency run one way. It is also the half worth
unit-testing: placeholder substitution and fallback are pure functions, testable
without rendering.

**An untranslated key is a build error, not a surprise in the UI.** `zh` is typed as
a total `Record` over the key set, so adding an English string without its
translation fails `npm run build`. What the compiler *cannot* catch is a translation
that drops a `{placeholder}` — that compiles and renders a sentence with a hole in
it — so a unit test asserts placeholder agreement across catalogues. Both catalogues
currently sit at 396 keys, verified equal.

The three overlays — search (`Mod+K`), the command palette (`Mod+P`), and the
workspace history browser — were the last surfaces holding literal English, and they
are now in the catalogue like everything else. Two rules came out of translating
them. A *rendered* group heading has to be a key, not a literal used only for
sorting; and a label built from a stored enum reads that enum's own translation
(`settings.density.*`) rather than interpolating the raw value, or a translated
sentence carries an English word in the slot the user is reading. Note also that a
component whose labels are translated must list `t` among its `useMemo`
dependencies: without it a language change leaves the labels the surface was opened
with while the filter keeps matching them, so typing hits a row whose visible text
no longer contains what was typed.

**No framework.** `next-intl` and `next-i18next` are built around locale-in-the-URL:
a `[locale]` segment, negotiating middleware, per-request catalogue loads on the
server. None of it applies. This is one user's loopback tool, its language sits
beside theme in `localStorage`, and every string lives in a client component.
Adopting that model would mean restructuring `app/` around a segment carrying a
value storage already holds. What a framework would genuinely buy — ICU
MessageFormat, per-locale splitting, plural and date rules — is either already
provided by `Intl` (see `lib/format.ts`) or not worth its weight at this size: both
catalogues together are a few kilobytes gzipped, less than the machinery to load one
lazily. Revisit at a third locale with translators who need a standard file format,
or past roughly a thousand keys.

Three details that are easy to get wrong:

- **Locale names are endonyms** (`English`, `中文`), so the picker's own labels are
  deliberately untranslated. Someone who has switched into a language they cannot
  read still needs a way back, which they only have if the entry they want is
  written in the language they know.
- **`<html lang>` syncs client-side** via `applyLocale`. The attribute is not
  decoration: a screen reader picks its voice from it, so a Chinese UI still tagged
  `en` is read aloud as English attempting Chinese. The document ships `lang="en"`,
  correct for the default.
- **Substitution is one regex pass, never a template literal or `eval`.** These
  values reach the DOM, so a substitution that could execute would turn a
  translation file into a script. An unmatched placeholder is left verbatim, which
  is visible in the UI and therefore gets fixed.

**A sentence with a value in a mono face cannot be a prefix key plus a suffix key.**
English precedes the value ("Inside `src/`"), Chinese wraps it ("在 `src/` 内"), so
the halves are not independently translatable and a translator cannot see the whole
sentence. `formatParts` splits the *translated* string around one placeholder
instead, keeping the sentence whole in the catalogue; `Around` in
`lib/file-operations.tsx` renders the result. An absent slot yields the whole string
as the prefix — visibly wrong rather than silently empty.

**Pure modules take `t` as a parameter rather than reading the locale.**
`describeWindow` and `describeBlocked` do, which moves their English plural rules
("1 row" vs "2 rows") into the catalogue where a translator can see them while
leaving both functions unit-testable. `describeKind` in `lib/file-kind.ts` returns a
`MessageKey` and lets the caller translate it, for the same reason. The one
exception is `next/dynamic`'s `loading` fallbacks: they are created at module scope
where no hook can run, so `LoadingKind` reads the locale itself.

**Backend `message` strings pass through untranslated, by design.** They are one
process's report of what it refused; localizing them would mean cataloguing every
string the API can emit. Both call sites say so in a comment.

## Real-Time Event Streaming

### SSE Connection Management

The stream is read with `fetch` and parsed in the client, **not** with
`EventSource`. This is a correction: the original design here specified
`EventSource` with one `addEventListener` per event kind, and the implementation
that followed it listened on `onmessage` instead — which by the SSE spec receives
only frames named `message`. Since the backend names every frame after its kind
(`event: thinking`), every mid-turn event was dropped, and the transcript
appeared all at once when the turn finished because the only repaint was the
history refetch on completion.

`EventSource` is the wrong primitive here for three reasons, not just that one:

1. Dispatch is by frame name, so a client must enumerate every kind it expects.
   A kind the backend adds later is silently dropped — and silent event loss
   renders identically to an agent that did nothing.
2. A frame named `error` (which the backend emits for a failed turn) arrives as
   the same `error` event the browser fires for a dropped socket, so a runner
   failure and a network failure cannot be distinguished.
3. It reconnects on its own with no way to advance `after_seq`, so resuming at a
   cursor requires fighting its built-in retry.

Reading the body directly fixes all three: the frame name is parsed but never
used for dispatch, so every frame reaches the consumer whatever it is called.

```ts
const response = await fetch(
  `/api/v1/turns/${turnId}/events?after_seq=${cursor}`,
  { credentials: "include", signal: controller.signal, cache: "no-store" },
)

const reader = response.body.getReader()
// `stream: true`: a chunk boundary can fall inside a multi-byte UTF-8 sequence.
const utf8 = new TextDecoder()
const decoder = createSSEDecoder()   // lib/sse-parse.ts

for (;;) {
  const { done, value } = await reader.read()
  if (done) break
  for (const frame of decoder.push(utf8.decode(value, { stream: true }))) {
    const event = JSON.parse(frame.data)
    if (event.seq <= cursor) continue   // drop replay overlap after reconnect
    cursor = event.seq
    enqueue(event)                      // coalesced, see below
  }
}
```

**Compression must be refused, or none of this streams.** The backend sets
`Cache-Control: no-cache, no-store, no-transform` on the event stream. The
`no-transform` directive is load-bearing rather than cosmetic: Next.js proxies
`/api/v1/*` through the standard `compression` middleware, which gzips any
compressible content type including `text/event-stream`, and a gzip stream cannot
be decoded until its block is flushed. With compression on, every event sat in
the compressor until the turn ended — while `curl`, which sends no
`Accept-Encoding`, saw perfectly paced frames and made the bug look like a client
problem. The header lives on the backend response rather than in
`next.config.ts` so the guarantee survives any proxy in front of the endpoint.

**Repaint is coalesced, not per-event.** Deltas arrive at token rate — the claude
runner measures ~20ms between tool-input deltas — so one `setState` per delta is
one React commit per token. Events are batched into a single flush per animation
frame (16ms), which bounds repaints by the display rather than by the model's
speed while staying far below the ~100ms at which a human perceives lag.

**Event normalization:**
- Parse JSON from `data` field
- Track sequence numbers for replay
- Distinguish `started` / `delta` / `completed` phases
- Merge deltas into live projection
- Persist only `completed` events

**Reconnection strategy:**
- Backoff: 500ms, 1s, 2s, 5s, 10s (capped, so a dead backend is polled gently)
- Resume from the highest `seq` seen, via `after_seq`
- Replay stable events from backend journal
- A clean end-of-body means the turn finished; retrying there would loop
- `AbortController` makes teardown immediate, which `EventSource` cannot offer

### Event Rendering

**Streaming text:**
```tsx
function StreamingText({ turnId, kind }: Props) {
  const liveText = useStreamStore(s => s.liveEvents.get(turnId)
    ?.filter(e => e.kind === kind && e.phase !== 'completed')
    ?.map(e => e.text)
    ?.join(''))
  
  const finalText = useQuery(['turns', turnId, kind])
  
  return (
    <div className="animate-typing">
      {liveText || finalText}
      {liveText && <Cursor />}
    </div>
  )
}
```

**Delta accumulation:**
- Buffer deltas in memory
- Render incrementally (typewriter effect)
- Replace with completed event when received
- Clear delta buffer after completion

**Scroll anchoring:**
- Auto-scroll when user is at bottom
- Lock scroll when user scrolls up
- Resume auto-scroll when user scrolls to bottom
- Smooth scroll during streaming

### Delegated trajectories

An event's `span` names the trajectory it ran in, and a `subagent` event's
`streamId` *is* a trajectory id (`docs/session-protocol.md` § Delegated
trajectories). `lib/timeline-tree.ts` turns that into a tree: `buildRows`
collapses deltas and pairs results, `buildTree` links each row to the trajectory
it names. Both are React-free so they unit-test in a `node` environment, which is
the same reason `lib/sse-parse.ts` is separate from the hook that uses it.

**The tree is navigated, not displayed.** Depth is unbounded — an agent delegates,
its delegates delegate, a dynamic workflow fans out to a dozen agents and then
synthesizes — so one indent level per depth runs out of viewport before it runs
out of tree. Instead:

- A trajectory renders as **one collapsed card**. Opening it shows its immediate
  children, themselves collapsed.
- Indentation stops at `MAX_INDENT = 2`. Past that a phone's content column is
  narrower than a line of code and a tool call's path truncates to nothing, so the
  indent stops conveying structure and starts destroying content.
- **Focus re-roots** the timeline at a trajectory with a breadcrumb back. This is
  the file-manager move, and it works because a user drilling into a delegation has
  stopped caring about its siblings.

**Collapsing is what makes it fast.** A closed card mounts none of its subtree, so
DOM cost tracks what the user opened rather than what the agent did: a
20,000-event workflow costs the same as a small one until someone drills in. An
indented rendering cannot have that property, because it must lay out every node
to know where any node goes. This is a stronger result than the memoization note
in `components/timeline.tsx`, and it is why virtualization is still not needed.

**Every card reports its whole subtree,** because collapsing is otherwise where a
failure goes to die. `Rollup` carries failures, nested trajectory count, tool
count, token usage, and duration, computed bottom-up; a descendant failure
escalates its ancestors' status even when their own result was clean. The rule is
that no state reachable in the tree is unreported at the root — otherwise
collapsing is lossy rather than deferred.

**`abandoned` is not `failed`.** A trajectory the server closed on turn
termination says nobody ever reported an outcome; a failed one says the work
reported an error. Colouring the first red would report an interrupted turn as a
broken one. Three consequences follow, and each was a real misreading of one real
session where a `Workflow` dispatched a backgrounded delegate and returned cleanly:

- **A server close is not a rollup failure.** The server marks its own closing
  event `isError` so the row reads as unresolved rather than clean — but counting
  that as a descendant failure escalated every ancestor, so a workflow whose own
  `tool_result` succeeded was badged red because one delegate was still running at
  turn end. `rowFailed` in `lib/timeline-tree.ts` therefore excludes a
  server-closed row, while the trajectory still reports `abandoned` as its own
  status. The distinction is erased one level *above* the node that holds it, which
  is why the rollup has to know about it.
- **"No interior" and "outlived its turn" are different claims, and only one is a
  measurement.** `no interior` means the host finished and forwarded none of its
  delegate's events — a real terminal state, and the common case for every
  delegating tool except `Task` (see § Delegation). Abandoned-and-empty is the
  opposite: it was still running when we stopped listening, so *nothing* is known
  about its interior. Rendering both badges together stated two things that cannot
  both be the point. Note that work the agent declares backgrounded is now exempt
  from the turn-settle sweep entirely, so `abandoned` no longer appears on a delegate
  that was *expected* to outlive its turn — see the roadmap's "Backgrounded work is
  not abandoned work".
- **A badge's explanation has to survive the behaviour it explains.** The
  "outlived its turn" tooltip said an agent *cannot* report back after its turn
  "because its process exits with the turn", and measured on `claude` 2.x that is
  false: stdout stays open, the delegate's completion arrives after its turn's
  `result`, and the agent opens an exchange of its own to report it — which is why
  `origin: "agent"` turns exist. So the badge explained its own state with the
  mechanism that resolves it, teaching a model of the product the product
  contradicts. What is true is narrower: nobody has reported an outcome *yet*.
  `abandoned` is not final, because a late outcome is written against the turn that
  owns the trajectory. The one terminal case is a restart — startup recovery
  deliberately ignores `is_backgrounded`, since a process that no longer exists cannot
  report — and that has to stay distinguishable from work still running in the
  background. The same wrong tense was in the overview's row description and in
  `ExtentBound`, and is corrected in both.
- **A delegate's summary was recorded and never rendered.** `task_notification`
  carries the delegate's own account of its work, and the card showed neither it nor
  `data.progress` — so a collapsed row was a status and a set of counts, which say
  work happened and nothing about what came of it. It is now the card's first line,
  clamped to two, and appended last to each overview row's accessible name so a reader
  scanning twenty rows hears the comparison before the prose. This matters most where
  a late outcome lands on a turn of the agent's own, because the summary is then the
  whole of what a user learns about work that finished after its dispatching turn.
  `trajectoryReport` in `lib/timeline-tree.ts` owns the two rules, both of which fail
  silently: a **promoted host's `text` is its tool arguments**, so its summary can only
  come from the `subagent` close held beside the row (`trajectoryClose`) — reading
  `text` uniformly captions a `Workflow` with a JSON blob, and reading only `text` left
  the `Agent` host, whose interior is never forwarded, with nothing at all. And a
  **close outranks progress**, because a snapshot describes a state the outcome has
  superseded.

  **Nor may it read a promoted host's `tool_result`**, which is the obvious next step
  precisely because a promoted host's *lifecycle* is its result (§ Promotion). A result
  is the tool's output, not the delegation's account of itself — and now that cards
  carry summaries, this is coupled to the suite: several
  `not.toContainText("running" | "failed")` assertions in `e2e/delegation.spec.ts` match
  a card's whole subtree, so a status word appearing inside a *summary* fails them as
  though the status were wrong. The fixture already holds the ammunition —
  `pr-host-d`'s result reads "dispatched; running in background". Pinned by a unit test
  rather than a comment, because a comment cannot fail: it was confirmed by adding the
  `row.result` fallback and watching the test reject that exact string.
- **A nameless delegation is called "Unnamed", and the rename order was the whole
  problem.** `trajectoryName` is the sole definition — it was duplicated in the card and
  the strip, which let one node be named two ways and a locator that found one stop
  finding the other. Its fallback used to be "Subagent", and that was wrong twice over.
  As *copy* it restated a category the chrome already carries, where "Unnamed" states
  what the row knows: nobody reported what this is — the same rule as the `≥` on a
  bounded duration. As a *string* it was a substring-superset of a tool name `claude`
  ships, so `"Subagent delegation"` contains `"agent delegation"` and every
  `{ name: "<tool> delegation" }` locator was ambiguous with it by construction — in
  exactly the states that render a nameless row, which is why it passed indefinitely and
  then failed twice under load, reading as a broken card. No tool name is a substring of
  "Unnamed delegation", so the class is closed rather than guarded per instance; the
  `exact: true` guards stay because `exact` is the correct expression of "this one
  region" regardless.

  **The ordering is the transferable part.** The adapter was *also* producing that
  string, sending `"Subagent"` as a wire `name` whenever a delegation's brief had not
  finished streaming — a data defect, not a display one, since `name` means "what this is
  called" and the guess is journaled: `consolidate` keeps the best name any phase gave a
  block, so a brief that never finishes leaves the placeholder as the trajectory's
  durable name. Renaming the client first would have given one condition two labels, the
  adapter's string for trajectories it named and the new one for the rest — worse than a
  single imprecise label, and invisible until a card renders. So: **when two layers both
  supply a default for one field, the layer that writes data stops first and the layer
  that renders changes second.** And the bound on the fix is worth stating, because it
  sounds better than it is — rows already journaled with the literal are permanently
  indistinguishable from a genuine name. There was nothing to migrate because nothing can
  tell them apart, so a historical "Subagent" row is not evidence an agent chose it.
- **A promoted host renders its own call.** The row keeps `kind: "tool_call"`
  precisely so the tool's name, arguments, and result survive promotion, so a card
  that drew only trajectory chrome answered opening a `Workflow` with an empty
  pane. The call renders above the work it dispatched, because that is the order it
  happened in — and a promoted host therefore stays expandable even with nothing
  forwarded, while a *declared* trajectory with no interior has nothing at all and
  keeps its disclosure suppressed. `aria-expanded` on a control with nothing to
  expand is a promise the tree cannot keep. **Focus is the same question one level
  up**: it re-roots to `children`, which is right for a declared `subagent` whose row
  carries no content of its own and wrong for a promoted host — focusing a `Workflow`
  whose interior was never forwarded showed a breadcrumb over an empty pane, which
  made a delegation that had genuinely reported nothing look broken. `Timeline`
  renders the focused row itself when it is not a `subagent`, and says why the pane
  is empty rather than leaving it blank.

**A rollup cannot compare siblings.** It describes one node against itself, so it
answers "should I open this branch" and not "which of these twenty was slow" — and a
user with a wide fan-out had to open every branch to find the expensive one, which is
the work collapsing was supposed to save. `lib/timeline-waterfall.ts` puts every
trajectory in view on **one shared time axis**, which is what distributed tracing
settled on for the same problem and the same `span`. A concurrent fan-out reads as
bars stacked in one band, a sequential chain as a staircase; the slow one is the long
bar. `components/delegation-overview.tsx` renders it as `left`/`width` percentages —
no chart library, no measuring pass — and it is `next/dynamic`, so a route that never
opens it pays nothing: `/sessions/[id]` measured 242.8 → 243.2 KB gzipped across the
strip and the summary line together, **+0.4 KB**. Note that
`scripts/measure-bundle.sh` reports the *root* route (221.1 KB), which is the figure
the roadmap tracks; a dynamic route renders no HTML at build time, so the session
route is measured by serving it and summing the scripts it references.

It is scoped to what is on screen, so under focus the comparison is between the
branch's own children rather than the whole turn, and it renders nothing below two
trajectories: one delegation compares to nothing, and its card already states its
duration. Rows are buttons whose accessible name carries what the bar shows — name,
status, elapsed, offset into the turn, depth — because a purely visual overview
excludes the reader who most needs a summary. Native tab order, not a roving cursor:
these are navigation targets like the breadcrumb's, and `lib/select-list.ts` exists so
a *choice list* does not grow a fourth cursor, not so a delegation is announced as an
option nobody picked.

**What it may not claim is the whole design.** Only `createdAtMs` exists, so three
states are not measurable and each is rendered as itself rather than as a bar:

- **Running** has no end. Deriving one from `Date.now()` would re-lay out every bar at
  clock rate inside a live stream, so the axis comes from the data and an open bar
  fades instead.
- **Abandoned** ends when the turn terminated and we stopped listening — a lower bound,
  labelled `≥`.
- **Consolidated** has no start at all. `history::consolidate` folds a declared
  trajectory's phases into one row keyed by its close and `block_to_event` publishes
  only that timestamp, so an *exact* duration exists only while a turn is live — which
  is a few seconds of the life of every turn a user scrolls back to. The interior
  survives, each descendant being its own timestamped block, so the row is bounded from
  below by the work it forwarded and says so. A trajectory that forwarded nothing has no
  bound either and is drawn as a caret, because a width there would be invented. The
  real fix is one field on the wire: `consolidate` already computes `started_at_ms` and
  stores it on the block, and publishing it would make these exact.

**"Slowest" therefore requires that every row is exact.** A rank over lower bounds is
not a rank: a delegate that ran 9s and forwarded its one event at 8.9s bounds at 0.1s,
so ranking it against an exact 0.5s names the wrong row — confidently, in answer to the
only question the view exists for. When any row is a bound the figures are still shown
with their `≥` and no row is badged. `e2e/timeline.spec.ts` asserts the live window and
the settled degradation separately, against a `fanout` fixture sized at six delegates
with genuinely different durations: at two the answer is visible without an overview,
and a fixture that emits its whole tree within one millisecond gives every bar the same
length, so neither could observe the comparison at all.

## Data Flow Patterns

### Creating a Session

```tsx
function CreateSessionDialog() {
  const createSession = useMutation({
    mutationFn: (data: CreateSessionRequest) =>
      api.sessions.create(data),
    onSuccess: (session) => {
      queryClient.invalidateQueries(['sessions'])
      router.push(`/sessions/${session.id}`)
    }
  })
  
  return (
    <Dialog>
      <form onSubmit={(e) => {
        e.preventDefault()
        createSession.mutate(formData)
      }}>
        <AgentSelect />
        <WorkdirInput />
        <FlagsInput />
        <Button type="submit">Create</Button>
      </form>
    </Dialog>
  )
}
```

### Sending a Message

```tsx
function MessageInput({ sessionId }: Props) {
  const sendMessage = useMutation({
    mutationFn: (data: CreateTurnRequest) =>
      api.turns.create(sessionId, data),
    onMutate: async (variables) => {
      // Optimistic update
      await queryClient.cancelQueries(['sessions', sessionId])
      const previous = queryClient.getQueryData(['sessions', sessionId])
      
      queryClient.setQueryData(['sessions', sessionId], (old) => ({
        ...old,
        turns: [...old.turns, {
          id: 'temp-' + Date.now(),
          status: 'queued',
          requestText: variables.text,
          createdAtMs: Date.now()
        }]
      }))
      
      return { previous }
    },
    onSuccess: (turn) => {
      // Start streaming
      streamManager.connect(turn.id)
    },
    onError: (err, variables, context) => {
      // Rollback
      queryClient.setQueryData(['sessions', sessionId], context.previous)
    }
  })
  
  return (
    <form onSubmit={(e) => {
      e.preventDefault()
      sendMessage.mutate({
        text: inputText,
        attachments,
        idempotencyKey: crypto.randomUUID()
      })
    }}>
      <textarea />
      <AttachmentPicker />
      <Button type="submit">Send</Button>
    </form>
  )
}
```

### Streaming Events

```tsx
function ConversationCanvas({ sessionId }: Props) {
  const { data: session } = useQuery(['sessions', sessionId])
  const currentTurn = session.turns.find(t => t.status === 'running')
  
  useTurnStream(currentTurn?.id, {
    onEvent: (event) => {
      // Update live projection
      streamStore.addEvent(currentTurn.id, event)
      
      // Finalize when completed
      if (event.phase === 'completed') {
        streamStore.finalizeEvent(currentTurn.id, event.seq)
        queryClient.invalidateQueries(['turns', currentTurn.id])
      }
    },
    onComplete: () => {
      queryClient.invalidateQueries(['sessions', sessionId])
    }
  })
  
  return (
    <div className="flex flex-col gap-4">
      {session.turns.map(turn => (
        <TurnView key={turn.id} turn={turn} />
      ))}
    </div>
  )
}
```

## Sylph-Specific Features

### There is no origin badge, and no reset banner

Both are deleted. They were the frontend half of a distinction the backend never made:
`origin` had **zero** behavioural branches in the backend, and the 24h-idle / 04:00-reset
policy the banner described was never scheduled by anything (`docs/architecture.md` § One
class of session).

Recorded because a reader coming from an older screenshot will look for them. The badge
rendered green-sylph / violet-native, and a banner promised a native session would not be
injected into or reset. The promise was kept by construction — nothing reset anything —
so the banner was describing an absence as a feature, on every one of those sessions,
forever.

What replaces them is smaller and says more.

### Two facets, each with a reason

A session's drivability is a property of its **link**, not of the session, and it has two
independent halves (`docs/architecture.md` § One link, two typed capability facets):

```tsx
// Not a boolean, and not a third link state. The vendors do not sort into three
// buckets: a terminal `pi` is observable and undrivable, `codex` is both, and
// `claude` is observable and semi-drivable behind a human approval gate.
function DriveState({ link }: { link: LinkState }) {
  if (link.control.available) return null

  const reason = {
    environment_unreachable: "This machine is offline. History reads; nothing can be sent.",
    not_ours_to_drive: "Running in a terminal. This agent offers no channel to a second party.",
    no_agent: "The agent CLI is not installed on that machine.",
  }[link.control.reason]

  return <Notice icon="eye">{reason}</Notice>
}
```

Two rules the composer must follow, both learned from shipped defects:

- **An empty capability surface means "we cannot reach it", never "there is nothing
  there".** Those coincide for a session Sylph launched and are opposite for one running
  under someone else.
- **Do not cache a failed reach forever.** `environment_unreachable` and
  `not_ours_to_drive` are cacheable — no retry wakes a laptop or teaches a vendor a new
  channel — but a failed handshake against a *reachable* environment must be allowed to go
  stale. Caching all of them under `staleTime: Infinity` made one failure permanent for
  the life of the page, which is what made a deterministic config problem present as
  intermittent.

### Fidelity gates what may be drawn

```tsx
// `stream` means every delta and its arrival time was recorded. `blocks` and
// `messages` mean the source recorded less, and no care recovers a timing that
// was never written — so a waterfall here would present equal-length bars as
// measurements. Show the marker; do not draw the chart.
{session.fidelity !== "stream" && <FidelityMarker value={session.fidelity} />}
```

Note what this is *not* correlated with: who started the session. A session Sylph
launched, read back from the vendor's store after its process died, is `messages` too.
Gating on anything else — an origin, a link state, whether the row came from a listing —
draws a duration that was never measured.


### Search integration

**One query, because there is one store.** This used to merge two result sets and
deduplicate them, and the deduplication is what is gone:

```tsx
function SessionSearch() {
  // No second source to merge, and no id translation. Search covers everything the
  // server holds, including sessions read out of an agent's native files, because
  // those are parsed into the same events and indexed by the same FTS table.
  const { data } = useQuery(
    ['search', query],
    () => api.search({ q: query }),
    { enabled: query.trim().length >= 2 },
  )

  return (
    <SearchInput
      placeholder="Search sessions…"
      results={data?.sessions ?? []}
      renderResult={(session) => <SessionCard session={session} />}
    />
  )
}
```

**What the old version had to do, and why not doing it is a correctness win as well as
less code.** It held a set of archive ids seen on live rows, plus a fallback set built by
re-deriving `<agent>:<native-id>` for live rows that had no archive id yet — a
translation between two id spaces. Comparing across those spaces produces **no error at
any layer** when it is wrong: the archive answers "no such session", the caller concludes
nothing matched, and every component behaves impeccably while the feature silently does
nothing. With identity now `(environment, agent, native_id)` throughout, that comparison
cannot be written.

**Keep it user-initiated.** FTS5 search measured **174–260 ms** while every other read is
microseconds to sub-millisecond, so `enabled` gating on a real query length is not
politeness — it is the difference between a search box and a load generator.

### History in the sidebar

The left pane is built from **one** source, and that is the change worth stating,
because it used to be two.

`GET /sessions` once held only sessions this app created, so a directory with years of
coding-agent history rendered as **"No sessions yet"** — the sidebar claimed a workspace
was empty while a separate archive held hundreds of sessions for that same path. The pane
therefore queried both and merged them, which brought a deduplication step, a
two-id-space fallback, and a count that could only be reported as `8+`.

With one store and one vocabulary (`docs/architecture.md` § One authority) all three go
away: a group is one query, rows are rows, and the count is a count.

What survives from that design, because it was about *presentation* rather than about
which store answered:

- **Marked by shape, not colour** (WCAG 1.4.1). What is marked has changed: there is no
  sylph/native distinction to draw. A row shows its **fidelity** when it is not `stream`,
  and `⑂N` for rolled-up delegated sessions.
- **`⑂N` counts only delegates that ran in *this* directory.** One that ran in a worktree
  or subdirectory is a row in that directory instead, because there it is the work rather
  than a delegation. Measured: 484 of 2238 delegates ran elsewhere, and folding those into
  an absent parent emptied 42 directories, one holding 193 sessions.
- **Fetched only while the group is open** (`enabled: open`), because each row costs the
  backend a lineage fold.
- **A distinguishable unreachable state.** "No history here" and "cannot reach the
  machine that holds it" must not render identically. What is unreachable is now an
  *environment* rather than a database, which is a better fact to show: it names a machine
  the user can wake.

### Browsing history: `Mod+Shift+K`

The sidebar shows a handful of rows per workspace; a real directory can hold over
a thousand sessions. Paging that in a 240px pane is not navigation, so
`components/history-overlay.tsx` is the browse surface: workspace-scoped, cursor
paged, with a client-side filter over the loaded pages.

Scoping is what separates it from `Mod+K`. Search answers "which session mentioned
X" across everything; this answers "what have I done in this project", which is
the question someone has when a workspace looks unfamiliar. The scope comes from
`activeWorkdir` in `lib/workspace-ui.tsx`, set by the open session or by the
sidebar group whose `Browse all` was clicked.

Filtering is deliberately local. Full-text search is already `Mod+K`, and re-querying it
per keystroke would run an FTS5 query per character — measured at 174–260 ms each
(`docs/api-design.md` § Search) — to answer a narrower question than the one already
loaded. The footer says the filter covers loaded sessions only.

**Paging is a footer control, not a row.** As the last child of the list it was both
invalid ARIA — a `listbox` admits only options — and below however many rows had
loaded, which is the one place a user paging a thousand sessions has to scroll to
reach. The footer is fixed, so it is one keystroke away at any scroll position.

Changing the scope goes through `WorkspacePicker`, not a `<select>`. On a real
archive that select is 200+ options sharing a `~/Workspace/` prefix, ordered by
recency and reachable only by first letter — and it could not name a directory the
archive has never seen at all. See **Choosing a place** for what the picker does
and why all three callers share it.

It is a **mode swap inside the same modal**, not a second modal. Nesting `Overlay`
in `Overlay` puts two focus traps on one DOM path: the outer trap's
`querySelectorAll` also matches the inner dialog's controls, so Tab at the
boundary depends on document order rather than on which dialog is on top. Swapping
the content keeps exactly one trap alive, makes "back" a state change rather than
an unmount, and leaves `Escape` meaning the same thing at every depth.

This overlay carries the one piece of lineage work the client owns: collapsing a
**fork** whose pair straddles a page boundary. A fork and its origin replay the
same prefix and read as one conversation, so only one belongs in the list. The
server drops the loser within a single response, but it keeps no state between
requests, and one measured pair sits 268 rows apart — so only a surface that
accumulates pages can see both. Rows arrive newest-first, so the survivor is
whichever appeared earlier, and the check runs in both directions: a row whose
`forkOf` names an earlier row is a duplicate, and so is a row that an earlier
row's `forkOf` names. Two index maps keep it linear as pages accumulate.

The sidebar deliberately does *not* do this: it fetches one page, where the server
has already resolved any pair.

### Reading an archived session

`app/archive/[...id]/page.tsx` renders an archived transcript read-only, through
the same `Timeline` the live conversation uses — the backend normalizes archived
events into the live `StreamEvent` shape, so there is no second renderer to drift.

A catch-all route, because a session id is the encoded triple and a native id can
itself contain a slash (`env:claude:uuid/agent-name`). Next hands those segments back
**still percent-encoded**, so each is decoded before rejoining; without that the client
re-encodes and the backend looks up a literal `env%3Aclaude%3A...`.

**Reading never mutates, and reading no longer implies a separate store.** A session
whose environment is offline, or whose vendor offers no second-party channel, opens and
renders like any other — what differs is the link's `control` facet, which reports
unavailable with a reason, so the composer explains itself instead of failing. There is
no "import to read" step, because there is nothing to import: the history is already the
server's.

#### Delegation is shown beside the transcript, not inside it

A live turn nests a subagent's work inside the tool call that spawned it, because every
event carries a `span`. **A record read from a vendor's native store carries none**, and
the page has to be built around that rather than around the resemblance.

This is a `fidelity` consequence, not an archive consequence, and it survives the move to
one store unchanged: no vendor writes event-level delegation structure to its native
files. Measured, the raw vendor payload field was null on all 36308 events sampled, and
the field that *looks* like a span means something different per agent — for `claude` it is
the conversation chain, where 13689 of 15119 pointers name the immediately preceding
event, so honouring it as a span nests a whole transcript into one chain thousands deep.
Its only sound use is pairing a `tool_result` to its `tool_call`.

So the page renders two session-level surfaces and nothing that imitates a
trajectory:

- **`LineageBar`**, above the transcript. `parentId` becomes "open parent session",
  because half a real archive is delegated work and a delegate reached from a search
  hit otherwise reads as a conversation someone started. `forkedFrom` gets its own
  wording — a fork replays its origin's whole prefix, so calling it a parent would
  claim a delegation that did not happen, and it is what explains a transcript that
  seems to open mid-conversation.
- **`Delegations`**, a list at the top of the pane, each row linking to that
  session's own transcript.

Placing that list *above* the transcript rather than at guessed points inside it is
the load-bearing choice. Attribution to the specific call that delegated was
measured: `claude` names a child `<parent>/agent-a<name>-<hash>`, but 1918 of 2104
suffixes in a real archive are hash-only, so anchoring recovers 8.8% and
misattributes on a collision. Nine in ten delegations would sit under the wrong tool
call while looking exactly like the live tree, and a reader has no way to tell the
two apart — so the panel states plainly that the archive records no link to the
exact tool call, which is the one thing that keeps a flat view honest rather than
merely poorer.

The list is not a disclosure inside a sidebar row, and that is an ARIA constraint
rather than a layout preference: a history-overlay row is a `role="option"`, which
may not contain a button, the same rule that stops a `role="menu"` holding a text
field. Session-level delegation therefore belongs on this page, where a `<section>`
with a heading and a list of links is valid.

E2E cannot reach any of this: the harness points `TRACEDB_BIN` at a path that does
not exist, so the archive is empty by construction and these surfaces never render.
They are covered by backend tests using **real** archive ids and by manual
verification against a 4922-session archive.

## Performance Optimization

### Bundle size management

**Current figure lives in `docs/roadmap.md`, and is measured by
`scripts/measure-bundle.sh`.** Do not quote a number here as current: this section
carried 203.9 KB as "measured" while the roadmap recorded 221.1 KB, which is the
same staleness the roadmap warns about, in a second place. What follows is the
attribution history — each figure is what a specific change cost when it landed, and
is correct as a past measurement.

The project maintains a compact bundle
through deliberate architecture and dependency choices. Phase 4 measured 187.8 KB;
Phase 6 added the shell, three overlays, the file browser, the Markdown renderer,
the diff view, and the fuzzy ranker for **+8.4 KB**, reaching 196.2 KB. Archive
browsing added **+1.2 KB** across 6.6 and 6.7, and the document viewers **+0.9 KB**:
the history overlay, the read-only archived view, and every format viewer are all
`next/dynamic`, so only their client-side entry points land here.

The design-system pass measured **198.2 KB → 203.9 KB**, `+5.7 KB`, with the same
recipe run on both trees — the baseline was re-measured rather than quoted, and
reproduced at 198.2 KB exactly. What it buys:

| Change | Cost |
| --- | --- |
| 48 hand-authored icons replacing Unicode dingbats | ~1.0 KB gz for the path data alone; the components tree-shake per route |
| `ui/button`, `ui/menu` — the shared primitives | The largest single item. `menu` carries popover placement, arrow navigation, and the sheet fork, and is reached from the sidebar, which *is* first-load |
| `lib/theme`, `lib/viewport`, `lib/use-dialog` | Small; two are `useSyncExternalStore` wrappers |
| Service-worker registration | ~0.2 KB, production-only |
| `Sheet`/`Drawer` split out of the shell | −0.2 KB. Small, but they are unreachable above `md`, so shipping them to every desktop route is wrong regardless of size |

That is a 2.9% increase for the token layer, dark mode, the icon set, the mobile
layout, and PWA installability. Judged against the stated policy — size should
scale proportionally to value delivered, and no arbitrary ceiling should constrain
valuable work — it is a straightforward trade. It is recorded rather than
optimised away because the alternative reading is available too: the primitives
would be cheaper if the sidebar were not in the first-load bundle, and moving it
behind a dynamic boundary is the change to make if this number needs to come down.

Replacing the all-sessions list with the start screen measured **198.4 KB → 198.2
KB** — the two tracks of this work were measured against each other after merging,
with the same recipe on both trees. The screen adds markup, the directory grouping,
and a small pure module; it gives back the four filter controls, the query-building
they fed, the debounce on that path, and a dead `sessionSubtitle` export, which is
slightly the larger side.

**The goal is to keep the bundle lean and fast-loading**, not to enforce a hard
numeric ceiling. As features grow, bundle size should scale proportionally to value
delivered. Monitor growth deliberately, optimize where reasonable, but never let an
arbitrary limit constrain valuable functionality or architectural improvements.

This principle explains the "no dependency" decisions throughout this document. The
alternative implementations — `react-markdown` + `remark-gfm` (~45 KB), Shiki or
Prism (~35 KB), jsdiff (~30 KB), Radix (~30 KB for the primitives used), fuse.js
(~12 KB), an icon package — would have totaled over 150 KB. Each replacement is a
bounded, well-understood algorithm carrying unit tests; that is the trade-off,
stated plainly. This approach remains correct while the implemented subset serves
the product's needs and the bundle stays compact. As requirements evolve, adopting
battle-tested libraries may become the better choice.

**Office formats are parsed server-side**, which is the largest single instance of
this. SheetJS (`xlsx`) is 7.5 MB unpacked, frozen at 0.18.5 on npm, and carries two
unpatched High CVEs — prototype pollution (CVE-2023-30533) and ReDoS
(CVE-2024-22363) — reachable on precisely the "parse a file we did not write" path
this feature needs. `calamine` and `quick-xml` do the same work in Rust, where the
cost is compile time rather than every page load, and the browser receives
structure instead of bytes. See `docs/api-design.md`, Files.

To measure:

```sh
cd frontend && npm run build
# then sum the gzipped size of the scripts referenced by a prerendered route
```

### Code splitting

`AppShell` sits in the root layout, so anything it imports statically lands in the
first-load bundle of *every* route. The three overlays and the Artifacts pane are
therefore `next/dynamic` with `ssr: false`: each is reached by a keypress or a
toggle, by which point the chunk has fetched. Measured effect: **204.3 KB → 195.8
KB**, which is the difference between over and under budget.

The format viewers split again *beneath* the pane, so opening a `.ts` file never
fetches the table or document code. With ~3 KB of headroom that is not an
optimization but the condition for the feature existing: the grid viewer was
confirmed absent from the first-load chunk set and fetched on demand at 1.5 KB.

### Render cost

**Measured: a 1000-event conversation cold-renders in well under the 2s budget**
(`e2e/timeline.spec.ts`, on a reload so it covers durable history rather than
incremental streaming).

The mechanism is memoization by object identity, not windowing. `buildRows`
produces a new row object only for rows whose content changed, and the row
component compares by reference — so a streaming token costs O(new rows) rather
than O(conversation length). A unit test pins that settled rows stay referentially
stable as events arrive, because that property is what the whole approach rests on
and it is invisible if it breaks.

**No virtualization.** A windowing library would consume most of the remaining
budget, and profiling showed re-render count, not DOM size, to be the cost. The
evidence that would reverse this: scroll jank on a large session while *idle*,
which is the signature of a DOM-size problem rather than a render-count one.

Bounded rendering keeps DOM size in check without windowing: code blocks show 60
lines with "show all", command output 40, diffs elide unchanged runs to three lines
of context, and diff alignment above 2000 lines per side degrades to a wholesale
replacement rather than allocating a quadratic table.

### Document previews

**A grid is not virtualized either, but for different reasons from the timeline,
and they were measured directly.** A sticky-header table in headless Chromium:

| Rows × cols | Cells | Build | Scroll |
| --- | --- | --- | --- |
| 200 × 30 (default window) | 6 231 | 46 ms | 0.06 ms/frame |
| 500 × 30 (max per request) | 15 531 | 82 ms | 0.06 ms/frame |
| 2000 × 30 | 62 031 | 302 ms | 0.22 ms/frame |
| 2000 × 50 | 102 051 | 517 ms | 0.18 ms/frame |

Neither axis behaves like the timeline's. A grid does not stream, so the
re-render count that memoization solves there is not a factor. And **scroll cost
is flat** across a 16× range of cell counts, because scrolling a sticky-header
table is compositor work that does not scale with row count — so the DOM-size
problem virtualization would fix does not appear at these sizes.

What does scale is the initial build. Two things keep it bounded:

- **The server windows the data** (200 rows by default, 500 max), so a 50 000-row
  sheet never arrives whole — it would be a ~50 MB payload and a ~600 ms parse.
- **Paging replaces the window rather than accumulating rows.** Accumulation was
  the first design and is wrong twice over: it needs the previous window as state,
  which forces either an effect that cascades a render or a ref read during
  render, and it grows the DOM without bound while `table-layout: auto` re-measures
  every cell in the table on each append. Replacing keeps the table at one
  window — 46 ms — no matter how far the user pages.

`table-layout: fixed` on wide sheets is load-bearing for the same reason: the
default `auto` algorithm resolves column widths by measuring every cell, making
layout cost global rather than proportional to what changed.

The evidence that would reverse this: scroll jank within a single window while
*idle*, the same signal named for the timeline above.

### File browsing

The Artifacts tree fetches **one directory per expansion**. Walking the tree
server-side would spend seconds on a 100k-file repository building a payload whose
first screen is all anyone reads. Each level owns its own query, so an expanded
subtree is cached independently and collapsing does not discard it. Listings cap at
2000 entries per directory and report `truncated`.

Previews cap at 1 MiB, truncating text to a 256 KiB prefix. Images are
all-or-nothing, because a truncated image decodes to nothing. Documents have their
own 25 MiB cap — a legitimate spreadsheet is routinely several MiB, so the 1 MiB
text bound would refuse the files people most want to look at.

Document queries are keyed on `(path, sheet, row)`, so switching back to a sheet
already viewed is a cache hit rather than another parse.

## Accessibility

Target: WCAG 2.1 AA. What that means concretely here, and how each part is
checked.

### Automated: axe-core, in CI

`e2e/accessibility.spec.ts` runs axe-core against every surface with real content —
the start screen in both its states (recents, and the first-run guide, which is a
different subtree reached by intercepting the listing), the create picker, a
conversation canvas holding tool calls, a diff, and a failed command with both
sidebars open, the composer mid-turn, and each of the three overlays while open.
`e2e/file-viewer.spec.ts` adds the file viewer in the pane and the fullscreen
dialog, since a data table and an iframe are both new surface. Tags: `wcag2a`,
`wcag2aa`, `wcag21a`, `wcag21aa`. **Currently zero violations.**

This is a floor, not a conformance claim. axe covers roughly a third of WCAG
criteria; whether focus order makes sense and whether an alternative is
*meaningful* are not machine-checkable. Full validation needs assistive-technology
testing and expert review, which has not been done.

The audit has already earned its place twice, catching a broken
`aria-activedescendant` that pointed at no element, and 2.48:1 text that had been
reviewed by eye and passed.

### Colour

`text-gray-400` (#99a1af) measures **2.48:1** on white and fails at every size, so
it is used only on `aria-hidden` decorative glyphs. Visible muted text is
`text-gray-500` minimum, and `text-gray-600` on a `gray-100` highlighted row where
gray-500 lands at 4.39:1 — just under the threshold. On the dark active sidebar
row the scale inverts: muted text there is `text-gray-300`, because gray-500 on
gray-900 measures 3.66:1.

Nothing is conveyed by colour alone (WCAG 1.4.1):

- Fidelity and link state: a shape plus an accessible name, never colour alone.
- Diff lines: a `+`/`−` sign as real text, plus an `sr-only` "added:"/"removed:"
  prefix — the glyph alone reads as punctuation to a screen reader.
- Tool failure: the word "failed" and the exit code, not a red border alone.

### Keyboard

Checked in a real browser, because none of it is verifiable statically:

- The skip link is the first focusable element, becomes visible on focus, and
  actually moves focus to `<main>`.
- Every overlay traps Tab, closes on Escape, and **returns focus to its opener** —
  without which a keyboard user is dropped at the top of the document.
- Bare-key shortcuts (`Shift+E`) do not fire while a text field has focus.
- Sidebars toggle, the palette runs commands, and search opens a session, all
  without a pointer.

### Semantics

- Landmarks: `<main id="main">`, `<nav aria-label="Sessions by workspace">`,
  `<aside aria-label="Workspaces">`, `<aside aria-label="Artifacts">`.
- Each tool call is a `<section>` with an `aria-label` naming its tool, so a screen
  reader can move between operations instead of reading one undifferentiated
  stream.
- Disclosures are native `<details>`/`<summary>`, which carry `aria-expanded` and
  keyboard operation with no script.
- Pickers follow the combobox pattern: focus stays on the input, rows carry
  `aria-selected`, and the input carries `aria-activedescendant`. This is the only
  arrangement where typing and arrow-navigating do not fight over focus. It is also
  mandatory rather than chosen — a `role="menu"` may not contain a text field. Every
  picker, the palette, search, and the history browser are now the same code, so this
  is one implementation rather than seven agreeing ones; see **One choice list**.
- Toggle buttons carry `aria-pressed`; the open session's sidebar link carries
  `aria-current="page"`.
- Streaming status is announced through `aria-live="polite"`.
- Markdown headings inside agent prose start at `<h4>`: the page owns h1–h3, and
  emitting an h1 mid-transcript would break the document outline that screen-reader
  navigation depends on.

### Not done

- Screen-reader passes with VoiceOver and NVDA.
- High-contrast and forced-colours modes.
- Dark theme — deliberately absent rather than half-built. It needs a `dark:` pass
  over every component plus a resolved `prefers-color-scheme` fallback, and a
  settings toggle storing a preference nothing reads would misrepresent that.

## Error Handling

### Error Boundaries

```tsx
class ErrorBoundary extends Component {
  state = { hasError: false, error: null }
  
  static getDerivedStateFromError(error) {
    return { hasError: true, error }
  }
  
  componentDidCatch(error, errorInfo) {
    console.error('Error boundary caught:', error, errorInfo)
  }
  
  render() {
    if (this.state.hasError) {
      return (
        <ErrorFallback
          error={this.state.error}
          reset={() => this.setState({ hasError: false })}
        />
      )
    }
    return this.props.children
  }
}
```

**Error boundaries wrap:**
- Each route (page-level recovery)
- Conversation canvas (isolated failure)
- Each turn (prevent cascade)

### Network Errors

**API call failures:**
```tsx
function useSessionMutation() {
  return useMutation({
    mutationFn: api.sessions.create,
    onError: (error) => {
      if (error.code === 'VALIDATION_ERROR') {
        toast.error(error.message)
      } else if (error.code === 'NOT_FOUND') {
        toast.error('Session not found')
        router.push('/')
      } else {
        toast.error('Failed to create session. Please try again.')
      }
    },
    retry: (failureCount, error) => {
      // Retry only on network errors, not validation
      return failureCount < 3 && !error.code
    }
  })
}
```

**SSE reconnection failures:**
- Show "Reconnecting..." badge after 3s
- Show "Connection lost" after 30s
- Offer manual refresh button
- Log errors to console for debugging

### User-Facing Error States

**Empty states.** The one empty state that matters — an app holding no sessions —
is not a placeholder card. It is the start screen's first-run guide, described under
"`/` — the start screen, not a session list", and the decision worth carrying is
that it is only rendered once a listing has *settled* empty. `selectStartState`
enforces that, because a pending or failed listing is also an empty array and
treating it as "no sessions yet" flashes onboarding at users who have hundreds.

**Loading states:**
```tsx
function ConversationCanvas({ sessionId }: Props) {
  const { data, isLoading, error } = useQuery(['sessions', sessionId])
  
  if (isLoading) {
    return <Skeleton />
  }
  
  if (error) {
    return (
      <ErrorState>
        <Icon name="alert-circle" />
        <Title>Failed to load session</Title>
        <Button onClick={() => queryClient.invalidateQueries()}>
          Try Again
        </Button>
      </ErrorState>
    )
  }
  // render conversation
}
```

## Testing Strategy

Three layers, split by what each can actually establish.

### Unit — Vitest (`npm test`)

```sh
cd frontend && npm test          # once
cd frontend && npm run test:watch
```

**Pure logic that replaced a dependency, plus the state decisions that are one line of
JSX away from being wrong.** That is the selection rule: hand-rolling a diff, a
Markdown parser, and a fuzzy ranker means taking on the test burden those libraries
carried, and this is where that debt is paid.

Counts are not quoted here. This section carried "140 tests" across three phases that
each added files, and a stale number in a table of *what is covered* is worse than
none — it invites a reader to conclude coverage shrank. `npm test` prints the total per
file; `docs/roadmap.md` § Current State carries the figure that is maintained
deliberately.

| File | Covers |
| --- | --- |
| `lib/diff.test.ts` | LCS alignment, line numbering against the original files, `-` before `+`, no phantom trailing line, prefix/suffix stripping speed, degradation above the size cap |
| `lib/markdown-parse.test.ts`, `components/markdown-render.test.ts` | Block and inline parsing, code not re-scanned, `javascript:`/`data:` links refused, termination on pathological input, and the rendered output |
| `lib/tool-shape.test.ts` | Each shipped agent's payload shape, name classification, hunk extraction, never throwing on malformed `data` |
| `lib/fuzzy.test.ts` | Subsequence matching and *relative ranking* — boundary and adjacency bonuses, field weighting |
| `lib/shortcuts.test.ts` | Canonical combo spelling, Cmd/Ctrl unification, recorder and dispatcher agreeing |
| `lib/sse-parse.test.ts` | Frame decoding across chunk boundaries — the bug `EventSource` shipped untested |
| `lib/timeline-rows.test.ts` | Delta accumulation, tool-result pairing, interleaved streams, referential stability of settled rows |
| `lib/timeline-tree.test.ts` | Delegation tree building: `span` attachment, orphans, cycles, promotion by use, status rollup, and `trajectoryName` including the nameless fallback |
| `lib/timeline-waterfall.test.ts` | The sibling comparison: shared time axis, exact extent vs lower bound, and which rows may be ranked |
| `lib/select-list.test.ts` | The one choice-list engine: one cursor per list, non-selectable rows, one current row per single-select group |
| `lib/request-form.test.ts`, `lib/request-parse.test.ts` | A declared request form's fields and the answer shape sent back |
| `lib/i18n/i18n.test.ts` | Message coverage across locales, so a translated surface cannot silently fall back |
| `lib/file-kind.test.ts`, `lib/grid-format.test.ts`, `lib/sandbox-html.test.ts` | Extension→viewer mapping, column/paging arithmetic, CSP injection for previewed HTML |
| `lib/welcome.test.ts` | The start screen's three states — that a pending or failed listing is never mistaken for an empty app — and the directory grouping behind recents |
| `lib/create-session.test.ts`, `lib/directory-picker.test.ts` | Dialog-free creation and default resolution; the folder chooser's filtering and typed-path path |

Tests live beside the code they cover, not in a parallel `tests/` tree, so a
reader of `lib/diff.ts` can see its tests without going looking.

`environment: "node"` — these are pure functions, so no DOM is needed. Anything
requiring one is tested in Playwright instead, where the DOM is real. `e2e/` is
excluded from Vitest because those files import `@playwright/test`, whose `test`
export would collide with Vitest's runner.

**No React Testing Library.** Component behaviour here is focus management,
keyboard dispatch, and streaming — all of which jsdom approximates badly enough
that a passing test would not mean much. Those live in Playwright.

### E2E — Playwright (`npm run e2e`)

**Two projects: `chromium` at desktop width, `mobile` on a phone profile.** Both
servers start automatically on offset ports with a throwaway data directory and a mock
adapter, so a run never touches the real journal, archive, or agents. `npx playwright
test --list` reports the totals; they are not quoted here for the same reason the unit
counts are not.

| File | Covers |
| --- | --- |
| `session-lifecycle.spec.ts` | Create, send, stream, reload-replay, sidebar grouping, resuming from `/` |
| `start-screen.spec.ts` | Usable before the listing settles, first-run guide vs recents, create button matching its shortcut, key hints reflecting a rebind |
| `timeline.spec.ts` | Tool calls as regions, inline diff, failure auto-expansion, Markdown, 1000-event render budget |
| `delegation.spec.ts` | The delegation tree: one card per delegate, nesting to the agent's own depth, a failure three levels down surfacing on the collapsed card above, focus mode and the way back, structure surviving reload, promotion of an undeclared delegating tool, a host that forwarded no interior saying so, the turn-settle sweep closing what an adapter left open without failing a host that succeeded, work reported on a later agent-initiated turn, and a backgrounded shell command staying a tool call |
| `requests.spec.ts` | A declared request form: pick, note, and multi-select reaching the agent; an answer outside the offered list; declining while a required field is empty; prefill; dismissal saying the agent still waits; keyboard-only answering |
| `workspace-shell.spec.ts` | Pane toggles and persistence, palette including grouping and a cursor that skips headings, search including an archive hit's match snippet, focus restoration, settings round-trip, Artifacts tree, history browser, dialog-free creation |
| `file-viewer.spec.ts` | Tree/viewer swap preserving expansion, fullscreen and focus return, dialog sizing to content vs filling for a page, single-modal invariant, CSV table, sandboxed HTML refusing script and cookie access, image zoom, axe over both scales |
| `streaming.spec.ts` | Mid-turn painting against a deliberately paced adapter |
| `workspace-picking.spec.ts` | The three "which directory" flows: filter focused on open, folder browsing filterable inside one focus trap, a typed path reaching an unlisted folder, Enter-opens vs `Mod+Enter`-chooses |
| `select-list.spec.ts` | The shared choice list itself: filter appearing only past the threshold, cursor opening on the current value, one checkmark per group, a refused write surfacing |
| `commands.spec.ts` | The slash menu after a completed turn, and a control command opening a picker rather than sending a message |
| `settings-config.spec.ts` | The Configuration panel reporting what *this* backend resolved, a missing path called out, the adapter directory actually in use marked among those tried, and the panel offering no way to edit a path from the browser |
| `file-operations.spec.ts` | Folder creation and its in-place errors, deletes that ask first, uploads including drop and overwrite, context menus, the raw route, and axe over both dialogs |
| `settings-identity-i18n.spec.ts` | Identity reporting when no `SOUL.md` resolves, language switching repainting and retagging (composer, skip link, file pane accessible names), a shortcut recorder refusing combos the browser keeps, and no *default* binding being one the browser would swallow |
| `accessibility.spec.ts` | Skip link, focus trapping and restoration including a picker's own field not becoming the node focus returns to, axe-core audit of every surface including both start-screen states and every overlay's list open |
| `mobile.spec.ts` | Phone-only: composer fits the viewport, sidebars out of flow, drawer and sheet behaviour, document cannot scroll, PWA installability, dark mode before paint, a long catalogue as a filterable sheet, a request form answerable by tapping, a three-deep delegation tree still fitting, the shortcut overlays' fields at or above 16px |

#### Why a second project exists

The three reported mobile failures — a composer wider than the viewport,
sidebars leaving ~150px for the conversation, and scrolling dragging the whole
shell into blank space — were all invisible to a 41-spec suite that was not
careless. Every spec in it ran at 1280px, where the same code is at worst
slightly cramped. A layout bug that manifests only below a breakpoint can only
be observed below that breakpoint, so a viewport is not an optional variant
here; it is the only instrument that sees the defect.

`devices["Pixel 7"]` rather than an iPhone profile. Only Chromium is installed
in this checkout, and an iPhone profile silently runs *in Chromium* anyway —
which would claim iOS coverage that does not exist. Pixel 7 is a Chromium
device, so what it emulates is what actually runs: `pointer: coarse`, a 412px
viewport, and a device pixel ratio above 1.

**What it therefore does not cover** is iOS Safari specifically: `100dvh` under
a collapsing toolbar, and zoom-on-small-input. Those are two of the mechanisms
behind the scroll bug, so the specs assert the fix through consequences a
Chromium phone *can* observe — nothing overflows the viewport, the document has
nothing to scroll, `window.scrollY` stays 0 after a 4000px wheel — rather than
by simulating a toolbar this browser does not have. A real device still wants a
pass.

**These specs assert invariants, not pixels.** A spec that knows the composer is
358px wide fails on the next padding change and says nothing about whether it is
usable. `overflowingElements()` instead reports every element whose box extends
past the right edge, which is both the actual defect and actionable when it
regresses.

**`npm run e2e` wipes `.e2e-home/` before the run**, and the script is the only place
that can. The journal was never cleared, and each spec's `mkdtemp` workdir becomes a
permanent sidebar group — grouping is by workdir, so a directory used once and deleted
still renders a group forever. At 151 groups (148 of them dead single-session
directories) the suite ran 35 minutes instead of 1.5, and two or three specs failed per
run on the 30s timeout: a different two or three each time, which reads as flakiness
rather than as accumulated fixture state. Raising the timeout does not help, because the
cost grows without bound. `SYLPH_E2E_KEEP_HOME=1` opts out to inspect a journal after a
failure; leaving it on brings both symptoms back.

**Neither Playwright hook can do the wipe, and both fail the same way.** A `globalSetup`
runs *after* `webServer`, because `webServer` is a plugin and plugin setup is ordered
first; module scope in `playwright.config.ts` runs again in every worker process. Both
therefore delete `journal.db` out from under a backend that has already opened it, and
the suite answers `unable to open database file` and 500s — the module-scope variant
eight seconds in, which reads as a mid-run database failure rather than as a fixture
wiping itself. `e2e/reset-home.mjs` runs from the npm script: one process, once, before
Playwright starts anything. A genuinely fresh home starts clean, so a first-run database
error is this bug and not a product one.

**The wipe removes the trigger, not the hazard.** A single run creates over a hundred
sessions of its own and the sidebar caps its listing at 200, so a spec late in the order
can still meet the cap — past which a new session *displaces* an older row instead of
adding one, and a "the group grew by one" assertion stays flat. Assert on identity
("the session I just created is listed here", matched by id), or count against a
`mkdtemp` directory of the spec's own.

**Clear the suite's ports before diagnosing a failure that happened before any spec
ran.** A backend surviving on 4318 from an interrupted run makes the suite fail at
startup, because `reuseExistingServer: false` correctly refuses to trust it. Playwright
names the cause (`is already used`), so the cost is only the time it takes to read the
message — but the same leftover-server class silently *measured* the wrong build in
`scripts/measure-bundle.sh`, which is the expensive shape.

```sh
for p in 4318 3101 3199; do lsof -ti tcp:$p | xargs kill 2>/dev/null; done
```

**`scripts/e2e-repeat.sh` separates a flake from a defect**, since a single red run
cannot. It runs the suite N times and reports each failing spec's frequency: every
run is `DEFECT`, some runs is `flake (3/10)`.

It reports `INCONCLUSIVE` when a run executed *no* tests — a backend that fails to
compile, a `webServer` that exits before any spec is reached. Such a run yields
`expected=0, failed=0`, which the summary originally read as "no failures in any
run": a false green, and the worst possible output from the one script whose job is
to tell you whether to trust a result. A run that never started is not evidence about
the code, and the script now says so instead of averaging it in.

**Two states are reached by intercepting the listing, not by emptying the
journal.** Specs share one backend and run serially, so by the time a spec runs
other sessions exist. `page.route` is how the empty-app and slow-listing branches
become deterministic, and it is the *client's* branch under test either way.

The E2E mock agent (a test-only `Adapter` implementation compiled into the envagent)
branches on its prompt to emit twelve scenarios: a plain streamed reply, a tool
sequence with a real diff and a failing command (`tools`), a 1000-event bulk
transcript (`bulk`), an approval, a `narrow` approval that forbids a lasting grant, a
question, an elicitation, a delegation, a promoted host (`promote`), an
agent-initiated turn (`unprompted`), `paced`, and `fanout`. Its payloads mirror what the
shipped adapters emit, transcribed from their source — a fixture that emitted a
tidier shape would test a protocol nothing speaks.

Four properties of this fixture are load-bearing rather than incidental, and each was
added because a green suite could not see a real defect without it.

- **`paced` spaces its stages a second apart**, because an adapter that finishes inside
  one render cannot distinguish streaming from batching.
- **`fanout` is wide and genuinely slow.** Six delegates with real sleeps, ~2.6s total.
  Width matters because the question the delegation overview answers is "which of these
  was slow", and with two delegates that is visible without any overview. Duration
  matters more: every other scenario emits its whole tree inside one millisecond, so
  the server stamps one `createdAtMs` on all of it and every bar is the same length —
  a client computing durations from the wrong field passed the whole suite.
- **It declares two select options in the same `model` category**, one with two choices
  and one with a long `CATALOGUE`. Below the filter threshold a config option is a
  `menu` of `menuitemradio`s and above it a combobox over a `listbox` — a different
  ARIA contract, not a different width. Both land on the composer's control row
  together, so one screen asserts both. A fixture offering only the short shape cannot
  observe a list-length bug at all.
- **`set_config` refuses one value on purpose**, so the rejected-write path stays
  covered. A mutation with no error path turns any backend refusal into a phantom UI
  bug: the sole observable outcome is that clicking a model does nothing.

**A note on shortcut tests.** Pressing a shortcut immediately after `page.goto`
races hydration: the listener is attached by an effect, so the key can land on
static markup. Tests wait for a client-rendered control first. This is a real race
on a cold load, not a test artifact.

## Development Workflow

### Setup

```sh
cd frontend && npm install
```

That is the whole setup. The scaffolding recipe that used to sit here —
`create-next-app`, then `npm install zustand @radix-ui/* framer-motion lucide-react
shiki react-markdown` — is deleted rather than corrected: **none of those seven
packages is a dependency**, and every one of them was rejected by name and with a
measured cost in **Technology Stack** above. A setup block installing them contradicted
the document it was in. `package.json` is the authority; today it is four runtime
dependencies (`next`, `react`, `react-dom`, `@tanstack/react-query`) and the toolchain.

### Development Server

```sh
# Terminal 1: Backend
cd backend && cargo run

# Terminal 2: Frontend
cd frontend && npm run dev
```

Frontend: `http://localhost:3000`  
Backend: `http://localhost:4317`

`cargo` is not on the default `PATH` here; use `~/.cargo/bin/cargo` or export it
first. For LAN or tunnel access, see `docs/development.md` — reaching `next dev` from a
non-localhost hostname needs `SYLPH_WEB_DEV_ORIGINS`, and without it every `/_next/*`
chunk 403s and the page freezes before React hydrates, which reads as a backend failure.

### Reading reference implementations

A per-component "read DSH first, then compare" checklist used to sit here, and it is
recorded in **Reference Implementations and Research** at the top of this file instead,
where it belongs: as a reference worth consulting, not a directive to follow. Three of
its instructions have since been overtaken by measurement — the canvas, the transport,
and the dependency choices — so a checklist telling a reader to mirror DSH's patterns
would send them at all three.

### API Proxy

`next.config.ts` rewrites `/api/v1/*` to the backend. **Not a route handler** — the
hand-written `app/api/[...proxy]/route.ts` this section used to show does not exist and
should not be added: a handler buffers, and buffering an SSE stream delivers a whole
turn at once.

Two properties are load-bearing, both documented at the rewrite itself:

- **Same-origin is required, not a convenience.** Auth is a `SameSite=Strict` cookie,
  which a browser will not send cross-origin to another port.
- **`SYLPH_WEB_BACKEND` is read at build time.** Next compiles rewrites into the route
  manifest, so it must be set for `next build` and not only for `next start`.

### Types

There is no code generation step. `lib/api.ts` is hand-written and mirrors the wire
shape exactly; `docs/api-v1.openapi.json` is the contract both sides are checked
against, and the backend has a test (`error::tests::codes_match_the_openapi_enum`)
pinning its error enum to it.

An `openapi-typescript` recipe used to sit here. It was never run: `openapi-typescript`
is not a dependency, there is no `src/types/api.ts`, and generating one would produce a
second definition of every shape that `lib/api.ts` already defines — two sources to
keep in step rather than one. The trade is deliberate: hand-writing means a wire change
surfaces as a type error in the client where it is consumed, and `npm run build` is the
gate.

### Build

```sh
cd frontend && npm run build        # also the TypeScript gate; no separate tsc step
npx eslint .                        # not `next lint`, removed in Next 16
npm test                            # Vitest
npm run e2e                         # Playwright, starts its own servers
```

`npm run build` emits `output: "standalone"`. That bundle omits `.next/static` and
`public`, so an install must copy both or the server starts cleanly and every asset
404s; `scripts/install.sh` does it and fails the install if `static` is missing. The
backend does not serve the frontend — there is no `dist/` to hand it.

<!--
  A per-week deliverables checklist used to live here. It has been removed rather
  than updated: `docs/roadmap.md` owns phase status, and a second copy in a design
  document only drifts from it. This file describes how the frontend is built and
  why; what is built and what is next belongs in the roadmap.
-->

## Future Enhancements

This list was written when most of it was unbuilt. What remains genuinely absent:

**Session management** — delete, move, and export have no endpoint behind them. Rename
has one (`PATCH /api/v1/sessions/{id}` accepts `title`) but no menu item calls it.
Forking at a turn would need an adapter able to resume from a prefix, which
the session protocol does not currently ask for.

**Search** — `Cmd+F` filters turns rather than highlighting matches inside them.
Highlighting means reaching into the Markdown, diff, and code renderers to rewrite
their text nodes, which is why it is not a small change.

**Lifecycle** — manual reset, native→Sylph conversion, and an incarnation history
viewer are all unbuilt. Conversion is still an open decision about endpoint shape, not
just missing UI; see `docs/roadmap.md`. Lifecycle timing (24h idle, 04:00) is modelled
in the backend and surfaced nowhere.

**Performance** — virtualization stays deferred, now with measurement behind it rather
than as an aspiration: a 1000-event conversation renders inside budget through
memoization. The evidence that would reverse it is scroll jank while *idle* on a large
session, which indicates DOM size rather than render count.

**Collaboration** — sharing, comments, and session-to-session diff remain out of
scope, and each would need a story for a surface that is currently loopback-only.

Already shipped, and listed here only because the original list predates them:
attachments with drag-and-drop and paste, image and PDF viewers, code rendering,
per-route code splitting, `Cmd+K` full-text search across live sessions and the
archive, inline diffs within a turn, **dark mode** (Phase 9, with a pre-paint bootstrap
— `docs/ui-layout.md` § Dark mode), **the mobile layout and PWA installability** (also
Phase 9, which is what makes the service worker a shipped thing rather than a
speculative one), and **the delegation tree with its sibling comparison** (Phase 12).

## Decision Log

### Why Next.js over Vite + React Router?

- SSR capability for future server components
- Built-in API routes for backend proxy
- Image optimization
- Better TypeScript integration
- Production-ready out of box

### Why TanStack Query over SWR?

- More powerful mutation handling
- Optimistic updates are first-class
- Better DevTools
- Wider ecosystem adoption

### Why no component library (neither Radix nor Headless UI)?

Radix would have been the right choice on accessibility defaults and edge-case
coverage. It was not taken because of one measured constraint: Phase 4 left ~12 KB
of first-load budget, and the primitives needed here — modal, context menu,
disclosure, listbox — cost roughly 30 KB of it.

What was built instead: `lib/use-dialog.ts` (focus trap, Escape, focus
restoration), `components/overlay.tsx` (the modal frame), `components/ui/menu.tsx`
(arrow navigation, viewport clamping, outside-dismiss, and the mobile sheet fork),
`components/ui/sheet.tsx` (drag-to-dismiss), native `<details>`, and the combobox
pattern in the two pickers. The axe audit and the focus tests exist to hold that decision
honest, and they have already caught one ARIA bug a library would not have had.

Reverse this if the budget rises or a primitive with real complexity is needed —
a date picker, a drag-and-drop tree, a virtualized listbox. Do not reverse it
piecemeal: a codebase with both patterns is worse than either.

### Why context and an external store, not Zustand?

Two contexts and one `useSyncExternalStore` cover the whole client state: shell
state that must outlive navigation, and preferences over `localStorage`. Zustand's
advantages — less boilerplate, selector-scoped re-renders — pay off at a scale this
store has not reached; it holds about a dozen fields.

`useSyncExternalStore` specifically, rather than state plus an effect: it avoids a
cascading render on mount, cannot mismatch during hydration because the server
snapshot is separate, and gets cross-tab sync from the `storage` event for free.

### Why hand-rolled Markdown, diff, and fuzzy matching?

See **Technology Stack** and **Performance Optimization**. Short version: the three
libraries total ~87 KB gzipped against ~12 KB of headroom, and what each provides
beyond the subset used — character-level diffs, patch formats, typo tolerance,
full CommonMark — is not rendered by this UI. The cost is carrying 81 unit tests
and two bugs already found in review; both are recorded in the test file that now
guards them.

## References

- Backend API: `docs/api-design.md`
- Event protocol: `docs/architecture.md` (Event Streaming section)
- Database schema: `docs/database-schema.md`
- Product roadmap: `docs/roadmap.md`
