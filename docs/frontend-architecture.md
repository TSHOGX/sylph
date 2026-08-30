# Frontend Architecture

How the frontend is *built*: state, streaming, data flow, performance, error handling,
and testing. `docs/ui-layout.md` is the companion and owns what the interface *is* — the
panes, the surfaces, the component inventory, and the design tokens.

It is an **agent workspace, not a chat client**: tool execution is the primary content,
prose is connective tissue, and navigation is keyboard-first.

Two things shape almost every decision below, so they are stated once here:

- **No runtime dependency is added without a measured reason.** The runtime tree is
  `next`, `react`, `react-dom`, and `@tanstack/react-query`; Markdown, diffing, fuzzy
  ranking, icons, and every UI primitive are implemented directly. § Technology Stack
  gives the cost of each library that was declined and what the replacement gives up,
  and § Performance Optimization gives the budget that motivates it.
- **A rule enforced in one place is stated in one place.** Where a mechanism lives in
  `lib/`, the comment beside it carries the mechanism and this document carries the
  principle.

## Design Philosophy

1. **Honest capability** — a session shows what can be observed, what can be driven, and
   how much resolution its record has. An empty capability surface means "Sylph cannot
   reach it", never "there is nothing there".
2. **Real-time first** — SSE-driven, with optimistic rendering where a mutation's outcome
   is predictable.
3. **Local-first** — no external runtime dependencies; works offline after first load.
4. **Accessibility** — WCAG 2.1 AA, keyboard-complete, screen-reader verified.
5. **Performance in service of the experience** — a compact bundle because the app must
   start quickly on a phone over a slow link, not to satisfy a number.
6. **Balance** — performance, experience, maintainability, and extensibility together;
   none of them alone.

Surfaces with no equivalent in a chat client, each of which exists because the backend
models something a chat client does not have: per-session agent, model, and effort read
from the agent's own report; history browsing and search with lineage rollup; link
capability facets reported separately with reasons; fidelity indicators, so a duration is
drawn only where one was measured; and an environment picker that says whether a machine
is reachable only through a relay.

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
that drops a `{placeholder}` — that compiles and renders a sentence with a hole in it — so
a unit test asserts placeholder agreement across catalogues.

Three rules that are easy to miss, each of which leaves a surface half-translated:

- **A *rendered* group heading has to be a key**, not a literal that happens to be used for
  sorting too.
- **A label built from a stored enum reads that enum's own translation**
  (`settings.density.*`) rather than interpolating the raw value, or a translated sentence
  carries an English word in the slot the user is reading.
- **A component whose labels are translated must list `t` among its `useMemo`
  dependencies.** Without it a language change leaves the labels the surface was opened
  with while the filter matches the new ones, so typing selects a row whose visible text no
  longer contains what was typed.

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

The stream is read with `fetch` and parsed in the client, **not** with `EventSource`.

`EventSource` is the wrong primitive here for three reasons:

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

### Event rendering

**Deltas accumulate into a live projection and are replaced by the `completed` event**,
which is the one that reaches the journal. `lib/timeline-rows.ts` owns the fold: it
collapses deltas into rows, pairs a `tool_result` with its `tool_call` by shared
`streamId`, and renders an unpaired result on its own so no output is ever dropped.

**Rows are memoized on event identity, and that is what makes the timeline fast** — see
§ Render cost, which also states the property a unit test pins and why it is invisible if
it breaks.

**Scroll is anchored only while the user is already at the bottom.** Auto-scrolling a user
who has scrolled up to read is the standard way to make a streaming transcript unusable.

### Delegated trajectories

An event's `span` names the trajectory it ran in, and a `subagent` event's `streamId` *is*
a trajectory id (`docs/session-protocol.md` § Delegated trajectories). `lib/timeline-tree.ts`
turns that into a tree — `buildRows` collapses deltas and pairs results, `buildTree` links
each row to the trajectory it names — and is React-free so it unit-tests in a `node`
environment, the same reason `lib/sse-parse.ts` is separate from the hook that uses it.

**The tree is navigated, not displayed.** Depth is unbounded, so one indent level per depth
runs out of viewport before it runs out of tree. Instead: a trajectory renders as one
collapsed card; indentation stops at `MAX_INDENT`, past which a phone's content column is
narrower than a line of code and the indent destroys content rather than conveying
structure; and focusing a trajectory **re-roots** the timeline with a breadcrumb back.

**Collapsing is what makes it fast**, and it is why virtualization is still not needed. A
closed card mounts none of its subtree, so DOM cost tracks what the user opened rather than
what the agent did — a huge workflow costs the same as a small one until someone drills in.
An indented rendering cannot have that property, because it must lay out every node to know
where any node goes.

**Every card reports its whole subtree.** `Rollup` carries failures, nested trajectory
count, tool count, token usage, and duration, computed bottom-up, and a descendant failure
escalates its ancestors' status even when their own result was clean. **No state reachable
in the tree may be unreported at the root** — otherwise collapsing is lossy rather than
deferred, and a user must open every branch to learn something broke, which is the work
collapsing was supposed to save.

#### `abandoned` is not `failed`

A trajectory the server closed on turn termination says *nobody reported an outcome*; a
failed one says the work reported an error. Colouring the first red reports an interrupted
turn as a broken one. Three consequences, each of which is a way the distinction gets erased
one level above the node that holds it:

- **A server-authored close is not a rollup failure.** The server marks its own closing
  event `isError` so the row reads as unresolved rather than clean — but counting that as a
  descendant failure badges every ancestor red, including a host whose own `tool_result`
  succeeded. `rowFailed` excludes a server-closed row while the trajectory still reports
  `abandoned` as its own status.
- **"Forwarded no interior" and "outlived its turn" are different claims, and only one is a
  measurement.** No interior means the host finished and forwarded nothing — a real terminal
  state. Abandoned-and-empty means it was still running when we stopped listening, so
  *nothing* is known about its interior. Rendering both badges states two things that cannot
  both be the point.
- **`abandoned` is not final**, because a late outcome is recorded against the turn that owns
  the trajectory. The one terminal case is a restart, since a process that no longer exists
  cannot report — and that must stay distinguishable from work still running in the
  background.

**A badge's explanation has to survive the behaviour it explains.** Saying an agent *cannot*
report after its turn because its process exits is false — stdout stays open, and the agent
opens a turn of its own to report (`docs/session-protocol.md` § An agent may start a turn).
What is true is narrower: nobody has reported an outcome *yet*.

#### A summary is the point of a collapsed card

A card showing only a status and counts says work happened and nothing about what came of
it. The delegate's own account is the card's first line, clamped, and is appended last to
each overview row's accessible name so a reader scanning many rows hears the comparison
before the prose. `trajectoryReport` owns two rules, both of which fail silently:

- **A promoted host's `text` is its tool arguments**, so a summary can only come from the
  `subagent` close held beside the row. Reading `text` uniformly captions a workflow with a
  JSON blob; reading only `text` leaves a host whose interior is never forwarded with
  nothing at all.
- **A close outranks progress**, because a progress snapshot describes a state the outcome
  has superseded.

**It may not read a promoted host's `tool_result`**, which is the obvious next step
precisely because a promoted host's *lifecycle* is its result. A result is the tool's
output, not the delegation's account of itself.

#### `trajectoryName` is the sole definition

Duplicating it lets one node be named two ways, so a locator that finds one stops finding
the other. Its fallback states what the row knows — nobody reported what this is — rather
than restating a category the chrome already carries.

**A fallback string must not be a substring of any real name.** A fallback that contains a
tool name makes every `{ name: "<tool> delegation" }` locator ambiguous with it *by
construction*, in exactly the states that render a nameless row — so it passes indefinitely
and then fails under load, reading as a broken card. Choosing a fallback no tool name
contains closes the class rather than guarding each instance.

#### A promoted host renders its own call

The row keeps `kind: "tool_call"` precisely so the tool's name, arguments, and result
survive promotion; a card drawing only trajectory chrome answers opening a workflow with an
empty pane. The call renders above the work it dispatched, because that is the order it
happened in.

So a promoted host stays expandable even with nothing forwarded, while a *declared*
trajectory with no interior keeps its disclosure suppressed — **`aria-expanded` on a control
with nothing to expand is a promise the tree cannot keep.** Focus is the same question one
level up: re-rooting to `children` is right for a declared `subagent` whose row carries no
content of its own and wrong for a promoted host, so `Timeline` renders the focused row
itself when it is not a `subagent`, and says why a pane is empty rather than leaving it
blank.

#### Comparing siblings needs one shared axis

A rollup describes one node against itself, so it answers "should I open this branch" and
not "which of these twenty was slow" — and a user with a wide fan-out had to open every
branch to find the expensive one. `lib/timeline-waterfall.ts` puts every trajectory in view
on **one shared time axis**, which is what distributed tracing settled on for the same
problem and the same `span`. A concurrent fan-out reads as bars stacked in one band, a
sequential chain as a staircase; the slow one is the long bar.

`components/delegation-overview.tsx` renders it as `left`/`width` percentages — no chart
library, no measuring pass — and is `next/dynamic`, so a route that never opens it pays
essentially nothing. It is scoped to what is on screen, so under focus the comparison is
between the branch's own children, and it renders nothing below two trajectories: one
delegation compares to nothing.

Rows are buttons whose accessible name carries what the bar shows — name, status, elapsed,
offset into the turn, depth — because a purely visual overview excludes the reader who most
needs a summary. Native tab order, not a roving cursor: these are navigation targets, and
`lib/select-list.ts` exists so a *choice list* does not grow a fourth cursor, not so a
delegation is announced as an option nobody picked.

**Most of the design is what it may not claim.** Three states are not measurable and each is
rendered as itself rather than as a bar:

- **Running** has no end. Deriving one from `Date.now()` would re-lay out every bar at clock
  rate inside a live stream, so the axis comes from the data and an open bar fades.
- **Abandoned** ends when the turn terminated and we stopped listening — a lower bound,
  labelled `≥`.
- **A trajectory that forwarded nothing** has no bound at all and is drawn as a caret,
  because a width there would be invented.

**"Slowest" therefore requires that every row is exact.** A rank over lower bounds is not a
rank: a delegate that ran nine seconds and forwarded its one event near the end bounds at a
fraction of that, so ranking it against an exact figure names the wrong row — confidently,
in answer to the only question the view exists for. When any row is a bound, figures are
shown with their `≥` and no row is badged.

## Data flow

Three flows carry almost everything, and each has one owner in `lib/`.

**Creating a session** — `lib/create-session.ts`. One mutation for every surface; the only
question is whether the caller already names a working directory (`docs/ui-layout.md`
§ Creating a session). On success it invalidates the session list and navigates to the new
session.

**Sending a turn** — `POST /sessions/{id}/turns` with an idempotency key, optimistically
appending the turn so the composer clears immediately, rolling back on error. The
idempotency key is what makes the retry safe rather than the optimism.

**Streaming a turn** — `lib/use-sse.ts` reads the body and hands frames to the page, which
merges them into the live projection and refetches history when the turn settles. The
handover from live stream to durable history is the subtle part: the live turn is filtered
out of history until the refetched history actually contains it, which closes the frame
where it would otherwise vanish from both.

**That refetch is sound because a terminal turn status means the record is finished** — a
server guarantee, not a race the client wins (`docs/database-schema.md` § A terminal status
means the record is finished). It is worth knowing it was not always: the turn row went
terminal before the server closed what the turn left open, so ending the claim there tore
down the stream before those closes arrived *and* refetched a journal that lacked them. Both
halves of the handover failed on the same early signal, which is why a delegation the server
had closed could render as running forever. A client needs no grace period here, and should
not grow one.

**A mutation with no error path turns any backend refusal into a phantom UI bug.** Where
the only observable outcome of a rejected write is that clicking did nothing, every layer
is behaving correctly and the layer that is *not* at fault is the one that gets debugged.
So a refused write invalidates and renders its error; the E2E fixture refuses one config
value on purpose to keep that path covered.

## Sylph-Specific Features

### There is no origin badge and no reset banner

Nothing in the UI says who started a session, because there is no such distinction to draw
(`docs/architecture.md` § One class of session). A reader coming from an older screenshot
will look for them.

The general rule they violated is worth keeping: **do not render an absence as a feature.**
A banner promising a session would not be reset described something nothing did, on every
such session, forever. What replaces both is smaller and says more — the two facets below,
and `fidelity`.

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

**One query, because there is one store.**

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
      results={data?.results ?? []}
      renderResult={(hit) => <SessionCard session={hit.session} />}
    />
  )
}
```

**One id space is what removes the deduplication**, and that is a correctness property
rather than a saving. Merging two result sets means translating ids between two spaces, and
such a comparison produces no error at any layer when it is wrong — one side answers "no
such session", the caller concludes nothing matched, and every component behaves impeccably
while the feature silently does nothing.

**Keep it user-initiated.** FTS5 search measured **174–260 ms** while every other read is
microseconds to sub-millisecond, so `enabled` gating on a real query length is not
politeness — it is the difference between a search box and a load generator.

### History in the sidebar

The left pane is built from **one** source.

A group is one query, rows are rows, and a count is a count — which follows from one
store and one vocabulary (`docs/architecture.md` § One authority). A sidebar built from two
sources needs a deduplication step, an id translation, and a count it can only report as
approximate (`8+`).

The presentation rules that matter:

- **Marked by shape, not colour** (WCAG 1.4.1). What is marked has changed: there is no
  sylph/native distinction to draw. A row shows its **fidelity** when it is not `stream`,
  and `⑂N` for rolled-up delegated sessions.
- **`⑂N` counts only delegates that ran in *this* directory.** One that ran in a worktree
  or subdirectory is a row in that directory instead, because there it is the work rather
  than a delegation. Measured, a substantial minority of delegates run elsewhere, and
  folding those into an absent parent emptied dozens of directories — one of them holding
  hundreds of sessions.
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

Changing the scope goes through `WorkspacePicker`, not a `<select>`. On a real store that
select is hundreds of options sharing a `~/Workspace/` prefix, ordered by recency and
reachable only by first letter — and it cannot name a directory no session has run in yet.
See `docs/ui-layout.md` § Choosing a place.

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

### Reading a session Sylph did not start

There is **no separate route and no second renderer.** An adopted session is
`/sessions/[id]` like any other, through the same `Timeline`, because an adapter parses
native history into the same events a live turn emits (`docs/architecture.md` § One
authority). There is no "import to read" step, because there is nothing to import.

What differs is not the route but two fields the renderer already handles: the link's
`control` facet reports unavailable with a reason, so the composer explains itself instead
of failing, and `fidelity` says how much resolution the record has.

**A session id in a route segment is percent-encoded and must be decoded before use.**
The id is the encoded triple and a native id can itself contain a slash, so Next hands
those segments back still encoded; re-encoding them produces a lookup for a literal
`env%3Aclaude%3A…`. `lib/use-session-id-param.ts` owns that decode.

#### Delegation is shown beside the transcript, not inside it

A live turn nests a subagent's work inside the tool call that spawned it, because every
event carries a `span`. **A record read from a vendor's native store carries none**, and
the page is built around that rather than around the resemblance.

This is a `fidelity` consequence, not a property of any one source: no vendor writes
event-level delegation structure to its native files, and the field that *looks* like a
parent pointer means something different per agent — for `claude` it is the conversation
chain, so honouring it as a `span` would nest a whole transcript into one chain thousands
deep. Its only sound use is pairing a `tool_result` to its `tool_call`.

So the page renders two **session-level** surfaces and nothing that imitates a trajectory:
`LineageBar` above the transcript, turning a parent id into "open parent session" and
giving a fork its own wording; and a `Delegations` list at the top of the pane, each row
linking to that session's own transcript.

**Placing that list above the transcript rather than at guessed points inside it is the
load-bearing choice.** Anchoring a delegation to the specific call that spawned it was
measured and rejected: most child ids carry only a hash, so anchoring recovers a small
fraction and misattributes on collisions — and nine in ten delegations sitting under the
wrong tool call, while looking exactly like the live tree, is worse than an honest flat
view. The panel says plainly that the source records no link to the exact call, which is
what keeps a flat view honest rather than merely poorer.

The list cannot be a disclosure inside a sidebar row, and that is an ARIA constraint
rather than a layout preference: a history-overlay row is a `role="option"`, which may not
contain a button — the same rule that stops a `role="menu"` holding a text field.

## Performance Optimization

### Bundle size management

**The budget exists to protect first paint on a phone over a slow link.** It is not a
score to keep down for its own sake, and it does not get to veto valuable work — but it is
the number that triggers a look, so growth is deliberate rather than discovered.

**No figure is quoted here.** `scripts/measure-bundle.sh` takes the measurement, and it
takes a `--route` because the routes are not comparable: the session route legitimately
carries the timeline, the delegation tree, and the overview, so quoting one route's figure
against another's budget has already caused confusion. `docs/roadmap.md` § Current state
carries the current numbers.

```sh
cd frontend && SYLPH_WEB_BACKEND=http://localhost:4317 npm run build
./scripts/measure-bundle.sh                          # the root route
./scripts/measure-bundle.sh --route='/sessions/[id]'
```

The script serves the route and reads the scripts its HTML references, rather than summing
the chunk directory. That is forced: summing counts every route's chunks together, a
prerendered route's served HTML is the only authority on what it actually loads, and a
dynamic route emits no build-time HTML at all while Turbopack publishes no per-route chunk
manifest for the App Router.

**What the budget buys, stated as the trade it is.** The libraries declined in
§ Technology Stack — a Markdown renderer, a syntax highlighter, a diff library, a
component primitives library, a fuzzy matcher, an icon package — total well over the
entire first-load budget. Each replacement is a bounded, well-understood algorithm
carrying unit tests, and that test burden *is* the cost. This stays the right trade while
the implemented subset serves the product; adopting a battle-tested library becomes
correct if the budget rises or a primitive with real complexity is needed.

**Office formats are parsed server-side**, which is the largest single instance. The
dominant JavaScript spreadsheet library is multiple megabytes unpacked, frozen on npm, and
carries unpatched high-severity CVEs — prototype pollution and ReDoS — reachable on
precisely the "parse a file we did not write" path this feature needs. `calamine` and
`quick-xml` do the same work in Rust, where the cost is compile time rather than every
page load, and the browser receives structure instead of bytes (`docs/api-design.md`
§ Files).

### Code splitting

`AppShell` sits in the root layout, so anything it imports statically lands in the
first-load bundle of *every* route. The three overlays and the Artifacts pane are
therefore `next/dynamic` with `ssr: false`: each is reached by a keypress or a
toggle, by which point the chunk has fetched. Measured, this is the difference between
being over the budget and under it — the single largest lever available.

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

- Systematic screen-reader passes with VoiceOver and NVDA. Ad-hoc VoiceOver testing has
  found real defects an automated checker cannot (see § Automated), so this is the
  highest-value gap.
- High-contrast and forced-colours modes.

## Error handling

**Error boundaries** wrap each route, the conversation canvas, and each turn, so one
malformed event cannot blank the app. `lib/error-boundary.tsx` is the implementation.

**A network failure and a refusal need different handling.** A validation error or a
`404` is final and is reported to the user; only a transport failure is retried. Retrying
a refusal turns one bad request into several and delays the message the user needs.

**SSE reconnection** backs off (500 ms through 10 s, capped, so a dead backend is polled
gently) and resumes from the highest `seq` seen via `after_seq`. A clean end of body means
the turn finished, and retrying there would loop forever. `AbortController` makes teardown
immediate.

**An empty state must not be reachable from a pending one.** A listing that has not
resolved is also an empty array, so treating "no rows" as "no sessions" flashes onboarding
at a user with hundreds of them. `selectStartState` distinguishes settled-empty from
pending and failed, and the start screen renders its first-run guide only for the first
(`docs/ui-layout.md` § Pending is not empty).

## Testing Strategy

Three layers, split by what each can actually establish.

### Unit — Vitest (`npm test`)

```sh
cd frontend && npm test          # once
cd frontend && npm run test:watch
```

**Pure logic that replaced a dependency, plus the state decisions that are one line of
JSX away from being wrong.** That is the selection rule: hand-rolling a diff, a Markdown
parser, and a fuzzy ranker means taking on the test burden those libraries carried, and
this is where that debt is paid.

`npm test` reports the totals. A count in a table of *what is covered* goes stale and then
invites a reader to conclude coverage shrank.

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
adapter, so a run never touches the real journal or the real agents. `npx playwright
test --list` reports the totals; they are not quoted here for the same reason the unit
counts are not.

| File | Covers |
| --- | --- |
| `session-lifecycle.spec.ts` | Create, send, stream, reload-replay, sidebar grouping, resuming from `/` |
| `start-screen.spec.ts` | Usable before the listing settles, first-run guide vs recents, create button matching its shortcut, key hints reflecting a rebind |
| `timeline.spec.ts` | Tool calls as regions, inline diff, failure auto-expansion, Markdown, 1000-event render budget |
| `delegation.spec.ts` | The delegation tree: one card per delegate, nesting to the agent's own depth, a failure three levels down surfacing on the collapsed card above, focus mode and the way back, structure surviving reload, promotion of an undeclared delegating tool, a host that forwarded no interior saying so, the turn-settle sweep closing what an adapter left open without failing a host that succeeded, work reported on a later agent-initiated turn, and a backgrounded shell command staying a tool call |
| `requests.spec.ts` | A declared request form: pick, note, and multi-select reaching the agent; an answer outside the offered list; declining while a required field is empty; prefill; dismissal saying the agent still waits; keyboard-only answering |
| `workspace-shell.spec.ts` | Pane toggles and persistence, palette including grouping and a cursor that skips headings, search including a hit's match snippet, focus restoration, settings round-trip, Artifacts tree, history browser, dialog-free creation |
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

That is the whole setup. `package.json` is the authority on dependencies: four at
runtime — `next`, `react`, `react-dom`, `@tanstack/react-query` — plus the toolchain.

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

### API Proxy

`next.config.ts` rewrites `/api/v1/*` to the backend. **Never a route handler**: a handler
buffers, and buffering an SSE stream delivers a whole turn at once.

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

**Deliberately no generated types.** Generating them produces a second definition of
every shape `lib/api.ts` already defines — two sources to keep in step rather than one.
Hand-writing means a wire change surfaces as a type error where the shape is consumed, and
`npm run build` is the gate.

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

## What is absent

**Session management** — delete, move, and export have no endpoint behind them. Rename has
one (`PATCH /api/v1/sessions/{id}` accepts `title`) but no menu item calls it. Forking at a
turn would need an adapter able to resume from a prefix, which the session protocol does
not ask for.

**Search highlighting** — `Mod+F` filters turns rather than highlighting matches inside
them. Highlighting means reaching into the Markdown, diff, and code renderers to rewrite
their text nodes, which is why it is not a small change.

**Collaboration** — sharing, comments, and session-to-session diff are out of scope, and
each needs a story for a surface that is currently loopback-only.

**Virtualization** stays deferred, with measurement behind it rather than as an
aspiration (§ Render cost). The evidence that would reverse it is scroll jank while *idle*
on a large session, which indicates DOM size rather than render count.

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

Radix would have been the right choice on accessibility defaults and edge-case coverage.
It was not taken because the primitives needed here — modal, context menu, disclosure,
listbox — cost several times the first-load headroom available when the decision was made.

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
libraries together cost several times the available headroom, and what each provides
beyond the subset used — character-level diffs, patch formats, typo tolerance, full
CommonMark — is not rendered by this UI. The cost is carrying the unit tests those
libraries would have carried.

## References

- What the interface is: `docs/ui-layout.md`
- Endpoint contracts: `docs/api-design.md`
- The event vocabulary an adapter emits: `docs/session-protocol.md`
- Tables and migrations: `docs/database-schema.md`
- What is built and what is next: `docs/roadmap.md`
