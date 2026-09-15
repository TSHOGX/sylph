# Frontend Architecture

[unified-trace-ui.md](unified-trace-ui.md) owns the target trace reader, data access,
interaction and accessibility contract. It follows the unified foundation and replaces
block/span interpretation and the former conversation/work reading rules below.
This document continues to own application framework, authentication transport, files,
internationalization and test tooling. Unreplaced code structure is implementation
context, not a reason to constrain the new trace model.

How the frontend is *built*: state, streaming, data flow, performance, error handling,
and testing. `docs/ui-layout.md` is the companion and owns what the interface *is* — the
panes, the surfaces, the component inventory, and the design tokens.

The trace reader lets authored conversation orient the work and exposes operations,
resources, agents and context progressively. Its semantic hierarchy is specified in
[unified-trace-ui.md](unified-trace-ui.md), independently of current components.

Two things shape almost every decision below, so they are stated once here:

- **No runtime dependency is added without a measured reason.** The runtime tree is
  `next`, `react`, `react-dom`, `@tanstack/react-query`, and `@tanstack/react-virtual`; Markdown, diffing, fuzzy
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
  parser (`lib/sse-parse.ts`); § SSE Connection Management states
  the three properties that reach.

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

Four roots, each with one responsibility. `docs/ui-layout.md` § Component Design covers what
the components do; this is the shape they sit in.

| Root | Holds | Owns |
| --- | --- | --- |
| `app/` | Four routes: start screen, search, `sessions/[id]`, and the root layout | Routing and the providers. A page renders its own scrolling content and nothing else |
| `components/` | The shell, the panes, the timeline and its renderers, the overlays, the composer, and `ui/` primitives | Everything that draws. `viewers/` splits by file family so a route pays only for the format it opens |
| `lib/` | The typed API client, the SSE frame parser, execution entities and paged windows, settings and shell state, and the pure algorithms that replaced dependencies | Every rule enforced in code, with the reasoning in the comment beside it. React-free wherever a unit test wants it |
| `e2e/` | Playwright specs and the reset script | Rendered behaviour, against a fixture agent |

Configuration sits at the root: `next.config.ts` (proxy rewrite, standalone output),
`vitest.config.ts`, `playwright.config.ts`. Unit tests live beside the code they cover
rather than in a parallel tree.

The rewrite proxy timeout is set in `frontend/next.config.ts` above the backend's
native-read deadline. Next defaults this socket timeout to thirty seconds, which can
close a healthy long-running history read with an empty proxy error before the backend
can answer. Query cancellation still releases the connection when the user navigates
away; the longer proxy deadline does not retain abandoned page reads.

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

Session detail, history, capabilities, pending requests, and directory reads consume
TanStack Query's abort signal. Leaving the session releases unfinished reads instead of
letting obsolete requests occupy the browser's connection pool. Directory expansion state
belongs to the selected session and resets on navigation.

Native-history import completion refreshes the affected session projections and file
queries as well as the listings. An explicit transcript reread refreshes that session's
file tree, including a cached failure. Import mode is a submitted request snapshot; an
active job displays the server's accepted mode, and reopening the panel reads that mode
from the job instead of inventing a one-time-import default. A failed import submission
retains its idempotency key for the same payload, including a manual retry of an automatic
import. Acceptance retires the key; another accepted run or a changed scope/mode receives
a new one.

An imported turn's `requestText` is the recorded user input even when no `user` event
exists. The transcript renders that input once, preserving independently journaled
steering messages. A turn containing no transcript content says so; its origin label
does not stand in for a missing message.

A failed capabilities request is an API error, not an unavailable facet. Only a
successful response can declare the environment disconnected. Failed reads show their
error with a read retry; they never synthesize an `environment_unreachable` reason.

**Control submissions:**
- The composer keeps its draft until the server accepts the request.
- A retry of that submission retains its idempotency key; another accepted submission,
  including identical text, receives a new key.
- Acceptance clears only the draft still matching the submitted snapshot. Text written
  while waiting remains in the composer.
- Control records are server state under `['session', id, 'controls']`. The session
  notification stream invalidates them on `control_changed` and reconnect.

### Client State

Two stores, neither of them Zustand.

**`lib/workspace-ui.tsx` — shell state, React context.** Which panes are open,
which overlay is up, which artifact the right pane shows, and the session-action
handler registration. This lives above the page tree because the shell outlives
navigation, and because a conversation event needs to ask the Artifacts pane to
open a file — a request that has to reach it from outside its own subtree.

**`lib/settings.tsx` — preferences, `useSyncExternalStore` over `localStorage`.**
Default agent and model, density, shortcut bindings, pane
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
that they do not follow a user to another browser. Account identity and environment
authorization are server-side; these preferences remain browser-local by design.

Pane visibility is read straight from settings rather than mirrored into local
state: the mirror needed an effect to adopt the stored value after hydration, and
could disagree with storage when a write failed.

### Internationalization

English is the source of truth and the fallback; Chinese is optional. The locale is a
settings field, so it rides the store above — switching language re-renders exactly the
components that read a translation.

```
lib/i18n/locale.ts   Locale set, tags, message lookup, format, formatParts.
                     No React, no settings import.
lib/i18n/messages.ts en — the `Dictionary` type and the source of truth
lib/i18n/zh.ts       zh — typed as a total `Dictionary`
lib/i18n/index.ts    "use client": useT(), useLocale(), re-exports
```

**The `locale.ts` / `index.ts` split makes the dependency run one way.** `lib/settings`
needs the `Locale` type to store and validate one, and `useT` needs `lib/settings` to know
which locale is active, so the primitives stay free of both React and the store. That half
is the testable one: substitution and fallback are pure functions.

**An untranslated key is a build error**, since `zh` is typed as a total `Record` over the
key set. Placeholder agreement across catalogues is a unit test, because dropping a
`{placeholder}` still type-checks.

Three rules keep a surface fully translated:

- **A rendered group heading is a key**, even when the same literal also drives sorting.
- **A label built from a stored enum reads that enum's own translation**
  (`settings.density.*`) rather than interpolating the raw value.
- **A component with translated labels lists `t` among its `useMemo` dependencies**, so a
  language change repaints the labels a filter matches against.
- **A sentence wrapping a value in a mono face is one key.** English precedes the value
  ("Inside `src/`") and Chinese wraps it ("在 `src/` 内"), so the halves are not
  independently translatable. `formatParts` splits the *translated* string around one
  placeholder; `Around` in `lib/file-operations.tsx` renders the result.
- **Pure modules take `t` as a parameter** rather than reading the locale, which puts their
  plural rules in the catalogue and keeps them unit-testable; `describeKind` in
  `lib/file-kind.ts` returns a `MessageKey` for the caller to translate. `next/dynamic`'s
  `loading` fallbacks are the exception, created at module scope where no hook can run.

**No framework.** `next-intl` and `next-i18next` are built around locale-in-the-URL — a
`[locale]` segment, negotiating middleware, per-request catalogue loads — which would mean
restructuring `app/` around a value `localStorage` already holds for one user's loopback
tool. What they otherwise buy is either already in `Intl` (`lib/format.ts`) or lighter to
inline: both catalogues together are smaller than the machinery to load one lazily.
Revisit at a third locale with translators who need a standard file format, or past
roughly a thousand keys.

Three details worth stating:

- **Locale names are endonyms** (`English`, `中文`), so the picker's labels stay
  untranslated: someone who has switched into a language they cannot read finds the way
  back only if the entry they want is written in the language they know.
- **`<html lang>` syncs client-side** via `applyLocale`, because a screen reader picks its
  voice from that attribute. The document ships `lang="en"` for the default.
- **Substitution is one regex pass**, since these values reach the DOM. An unmatched
  placeholder is left verbatim, so it surfaces in the UI.

**Backend `message` strings pass through untranslated.** They are one process's report of
what it refused, and localizing them would mean cataloguing every string the API can emit.

## Real-Time Event Streaming

### SSE Connection Management

Notification and explicit replay streams are read with `fetch` and parsed in the client.
The interactive timeline uses bounded revision reads (§ Event rendering).
Reading SSE directly makes three
properties reachable. `EventSource` dispatches by frame name, so a client would have to
enumerate every kind and a kind added later would be dropped silently; a frame named `error`
would arrive as the same event the browser fires for a dropped socket; and its built-in
retry cannot advance `after_seq`. Reading the body parses the frame name without dispatching
on it, so every frame reaches the consumer whatever it is called, and the parser is
unit-testable on its own.

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
    enqueue(event)                      // explicit replay consumer
  }
}
```

**Compression must be refused, or none of this streams.** The backend sets `Cache-Control:
no-cache, no-store, no-transform` on the event stream. `no-transform` is what forbids the
re-encoding: a proxy that gzips `text/event-stream` holds every event in the compressor
until its block flushes, and the whole turn then lands at once. The header belongs on the
backend response so the guarantee survives any proxy, tunnel, or nginx in front of the
endpoint.

**Timeline updates are coalesced by projection revision.** Polling reads the latest
revision of changed nodes within a bounded page. Token-rate events do not replay or fold
the transcript in the browser; each changed entity notifies its own mounted row.

Reconnection is covered in § Error handling.

### Turn lifecycle

Read independently evidenced Activation outcomes, source coverage and projection progress.
Terminal execution does not end trace subscriptions or imply that every result arrived.
See [unified-trace-ui.md](unified-trace-ui.md).

### Event rendering

The browser consumes normalized objects and bounded server projections; it does not parse
vendor JSON, infer authorship, or promote tool hosts into agents. The replacement data
contract is [unified-trace-storage.md](unified-trace-storage.md#read-protocol), and reader
behavior is [unified-trace-ui.md](unified-trace-ui.md).

### Delegated trajectories

Root and child AgentThreads use one reader. Independent Operation/Resource lifecycles,
real child identities and scoped delivery evidence replace the span/promotion renderer.
See [unified-trace-ui.md](unified-trace-ui.md).

#### Rendering `abandoned` distinctly from `failed`

The target reader distinguishes own outcome from lost observation and unresolved current
state. It consumes those separate facts; a parent close cannot mark independent children
abandoned. See [the lifecycle contract](unified-trace-foundation.md#independent-lifecycle-claims).

#### A summary is the point of a collapsed card

Summaries are bounded query projections over typed operations/resources. They never
become archival content or the source of an object's identity/outcome.
See [unified-trace-ui.md](unified-trace-ui.md).

#### `trajectoryName` is the sole definition

The replacement reads evidenced optional agent names and operation descriptions from
normalized objects. Fallback labels are presentation only, and names never resolve identity.
See [unified-trace-ui.md](unified-trace-ui.md).

#### A promoted host renders its own call

Promotion is removed. A host invocation remains an Operation, with independently proved
nested work and child links. See [unified-trace-foundation.md](unified-trace-foundation.md#operation-contract).

#### Comparing siblings needs one shared axis

Comparable interval views require compatible clock domains, covered endpoints and explicit
scope. A visual common axis cannot manufacture those facts.
See [unified-trace-foundation.md](unified-trace-foundation.md#ordering-clocks-and-measurements).

## Data flow

Three flows carry almost everything, and each has one owner in `lib/`.

**Creating a session** — `lib/create-session.ts`. One mutation for every surface; the only
question is whether the caller already names a working directory (`docs/ui-layout.md`
§ Creating a session). On success it invalidates the session list and navigates to the new
session.

**Sending or steering a turn, or queueing a follow-up** — every submission has a stable
idempotency key and a server control record. The composer clears after acceptance, guarded
against overwriting a newer draft. A failed request keeps both its text and retry identity.
The dynamically loaded queue panel reads the shared resource, including immutable dispatched
rows and failure details. Edits and cancellation carry the displayed revision, so concurrent
changes produce a visible conflict followed by a resource refresh. Rendering is paged within
the bounded recent ledger, while every pending item remains reachable.

**Streaming a turn** — `lib/execution-store.ts` merges revision pages into stable entities.
Consolidation preserves those addresses, so settling a turn does not switch the renderer
between separate live and historical collections. Control state has its own bounded
metadata read; it never hydrates transcript bodies.

**That refetch is sound because a terminal turn status means the record is finished** — a
server guarantee rather than a race the client wins (`docs/database-schema.md` § A terminal
status means the record is finished). A client therefore needs no grace period here.

**A refused write invalidates and renders its error.** Where the only observable outcome of a
rejected write is that clicking did nothing, the layer at fault is invisible. The E2E fixture
refuses one config value on purpose to keep that path covered.

## Sylph-Specific Features

### What the UI does not draw

Nothing says who started a session, because there is no such distinction
(`docs/architecture.md` § One class of session), and nothing promises a session will not be
reset. **Do not render an absence as a feature.** The two facets below and `fidelity` carry
what those markers were reaching for.

### Two facets, each with a reason — and one field that is not a facet

A session's drivability is a property of its **link**, not of the session, and it has two
independent halves (`docs/architecture.md` § One link, two typed capability facets). But
the composer does not read the facet directly: it reads `controlBlock`, the server-derived
answer that folds the caller's authorization together with Sylph's reach.

```tsx
// Not a boolean, and not a third link state. Drivability varies by vendor and by how
// the session started: a terminal-adopted `claude` is observable and reports
// `not_ours_to_drive`, since its control plane has no transport channel, while the same
// agent launched by Sylph runs over stdio and reports `available`.
//
// Read `controlBlock`, not `link.control`. A facet says whether *Sylph* can reach the
// environment — the same answer for every observer — while a grant says whether *you*
// may drive it. Folding them here would put the disclosure order in the renderer, and
// each renderer would pick its own.
function DriveState({ block }: { block: ControlBlock | null }) {
  if (!block) return null

  if (block.kind === "unauthorized") {
    // Deliberately says nothing about the machine: an unauthorized caller has no
    // standing to learn whether someone else's laptop is awake.
    return (
      <Notice icon="lock">
        {{
          not_granted: "You do not have control access to this environment.",
          expired: "Your control access to this environment has expired.",
        }[block.reason]}
      </Notice>
    )
  }

  return (
    <Notice icon="eye">
      {{
        environment_unreachable: "This machine is offline. History reads; nothing can be sent.",
        not_ours_to_drive: "Running in a terminal. This agent offers no channel to a second party.",
        no_agent: "The agent CLI is not installed on that machine.",
        handshake_failed: "The agent would not start on that machine.",
      }[block.reason]}
    </Notice>
  )
}
```

Two rules bind the composer:

- **An empty capability surface means Sylph cannot reach the session**, never that there is
  nothing there.
- **Cacheability comes from `is_retryable` on the wire, never from a list here.** A reason a
  retry cannot change is cacheable; one that a waking laptop or a redialing envagent can
  change must be allowed to go stale. `authorization` and `controlBlock` are outside that
  rule entirely and are never cached, since a stale grant is a permission outliving its
  issue.

### Fidelity gates what may be drawn

Coarse fidelity is a record summary. Each duration/rendered interval additionally checks
its own timing/coverage evidence. See [unified-trace-ui.md](unified-trace-ui.md).

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
(`docs/api/sessions.md` § Search) — to answer a narrower question than the one already
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

**The picker asks to follow the store, never to snapshot it.** `POST /sessions/adopt` takes
`watch`, and this client always sends `true` — the reasoning, and the two rejected
alternatives, are in `docs/environments.md` § A product surface bringing a session in.
What that costs the frontend is one string: adopting has to *say* it keeps up, because
"adopt" reads as a one-time import and a user who believes that has no reason to keep
working in the terminal.

**A session id in a route segment is percent-encoded and must be decoded before use.**
The id is the encoded triple and a native id can itself contain a slash, so Next hands
those segments back still encoded; re-encoding them produces a lookup for a literal
`env%3Aclaude%3A…`. `lib/use-session-id-param.ts` owns that decode.

#### Delegation is shown beside the transcript, not inside it

The agent directory and focused/companion reader are projections over actual AgentThreads;
interaction placement is specified in [unified-trace-ui.md](unified-trace-ui.md), not inferred
from nested execution containers.

## Performance Optimization

### Bundle size management

**The budget exists to protect first paint on a phone over a slow link.** It is the number
that triggers a look, so growth is deliberate.

**Current numbers come from `scripts/measure-bundle.sh`**, which takes a `--route` because
routes are not comparable: the session route legitimately carries the timeline, the
delegation tree, and the overview. A figure quoted in prose here would be a second copy of
a value that moves with every dependency change.

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

**It refuses a busy port rather than measuring whatever holds it.** Killing a subshell does
not kill what it exec'd, so a backgrounded `next start` survives and serves its own older
build, whose HTML names chunk hashes the current build never produced. Three things together
close that class: run the job in its own process group (`set -m`) and kill the group, refuse
a busy port, and make a missing artifact name its likely cause. `lsof -ti tcp:PORT` answers
it directly whenever output describes a tree that does not match disk.

**The budget buys a trade.** The libraries declined in § Technology Stack total well over
the entire first-load budget; each replacement is a bounded algorithm carrying unit tests,
and that test burden is the cost. Adopting a battle-tested library becomes correct once the
budget rises or a primitive with real complexity is needed.

**Office formats are parsed server-side**, which is the largest single instance. The
dominant JavaScript spreadsheet library is multiple megabytes unpacked, frozen on npm, and
carries unpatched high-severity CVEs — prototype pollution and ReDoS — reachable on
precisely the "parse a file we did not write" path this feature needs. `calamine` and
`quick-xml` do the same work in Rust, where the cost is compile time rather than every
page load, and the browser receives structure instead of bytes (`docs/api/files.md`
§ `GET /api/v1/sessions/{id}/files/document`).

### Code splitting

`AppShell` sits in the root layout, so anything it imports statically lands in the
first-load bundle of *every* route. The three overlays and the Artifacts pane are
therefore `next/dynamic` with `ssr: false`: each is reached by a keypress or a
toggle, by which point the chunk has fetched. Measured, this is the difference between
being over the budget and under it — the single largest lever available.

The format viewers split again *beneath* the pane, so opening a `.ts` file never
fetches the table or document code. Headroom against the budget is small enough that this
is the condition for the feature existing rather than an optimization: the grid viewer was
confirmed absent from the first-load chunk set and fetched on demand.

### Render cost

First-screen work is bounded by the node and byte limits in `docs/storage.md` § Execution
node projection. Collapsed execution mounts no descendants. Each parent caches a bounded
window, and only the currently expanded visible list enters TanStack Virtual. Its end-anchor
mode preserves reading position across prepend and resize, and follows appends only near
the bottom. A structural update captures the visible node before notifying React; resize
corrections preserve that node even when controls leave space above it. The next user
gesture releases that anchor. Native scroll anchoring is disabled so the browser cannot
apply a second adjustment. Tests measure entity identity, subscriber notifications, body requests and
mounted rows separately from HTTP latency.

### Document previews

**A grid is not virtualized, unlike the timeline,
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
- **Paging replaces the window rather than accumulating rows**, which keeps the table at one
  window — 46 ms — however far the user pages. Accumulating needs the previous window as
  state, forcing either an effect that cascades a render or a ref read during render, and it
  grows the DOM without bound while `table-layout: auto` re-measures every cell on each
  append.

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
`wcag2aa`, `wcag21a`, `wcag21aa`. The suite fails on any violation, so the current
count is whatever `npm run e2e` reports.

This is a floor, not a conformance claim. axe covers roughly a third of WCAG criteria;
whether focus order makes sense and whether an alternative is *meaningful* are not
machine-checkable. Full validation needs assistive-technology testing and expert review,
which has not been done.

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
- Bare-key shortcuts (`Shift+C`) do not fire while a text field has focus.
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
  is one implementation rather than seven agreeing ones
  (`docs/ui-layout.md` § One choice list).
- Toggle buttons carry `aria-pressed`; the open session's sidebar link carries
  `aria-current="page"`.
- Streaming status is announced through `aria-live="polite"`.
- Markdown headings inside agent prose start at `<h4>`: the page owns h1–h3, and
  emitting an h1 mid-transcript would break the document outline that screen-reader
  navigation depends on.

### Not done

- Systematic screen-reader passes with VoiceOver and NVDA. This is the highest-value gap,
  since ad-hoc passes find defects an automated checker cannot.
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

Session notifications have no replay cursor. Their connection retries both network failures
and clean EOF with capped backoff, and every recovered connection invalidates session,
history, capabilities, and requests. This resource reconciliation recovers notifications
missed while offline even when a successful capability response was cached indefinitely.

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
| `lib/tool-shape.test.ts` | Server-authored tool descriptions, complete hunk extraction, and malformed `data`; vendor classification and real adapter contracts live in `crates/server/src/events/tool_summary.rs` and `crates/server/tests/timeline_nodes.rs` |
| `lib/fuzzy.test.ts` | Subsequence matching and *relative ranking* — boundary and adjacency bonuses, field weighting |
| `lib/shortcuts.test.ts` | Canonical combo spelling, Cmd/Ctrl unification, recorder and dispatcher agreeing |
| `lib/sse-parse.test.ts` | Frame decoding across chunk boundaries — the bug `EventSource` shipped untested |
| `lib/execution-store.test.ts` | Entity identity, subscriber isolation, snapshot merging, parent removals, and bounded window eviction |
| `lib/select-list.test.ts` | The one choice-list engine: one cursor per list, non-selectable rows, one current row per single-select group |
| `lib/request-form.test.ts`, `lib/request-parse.test.ts` | A declared request form's fields and the answer shape sent back |
| `lib/i18n/i18n.test.ts` | Message coverage across locales, so a translated surface cannot silently fall back |
| `lib/file-kind.test.ts`, `lib/grid-format.test.ts`, `lib/sandbox-html.test.ts` | Extension→viewer mapping, column/paging arithmetic, CSP injection for previewed HTML |
| `lib/welcome.test.ts` | The start screen's three states — that a pending or failed listing is never mistaken for an empty app — and the directory grouping behind recents |
| `lib/create-session.test.ts`, `lib/directory-picker.test.ts` | Dialog-free creation and default resolution; the folder chooser's filtering and typed-path path |
| `lib/adopt-request.test.ts` | The adopt payload: `watch` present and true for every row regardless of what the listing recorded, the identity triple carried as one unit, and nothing sent that the backend derives from the store |

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
| `timeline.spec.ts` | Three-level disclosures, Markdown order, keyboard access, and bounded reload rendering |
| `delegation.spec.ts` | The delegation tree: one card per delegate, nesting to the agent's own depth, a failure three levels down surfacing on the collapsed card above, focus mode and the way back, structure surviving reload, promotion of an undeclared delegating tool, a host that forwarded no interior saying so, the turn-settle sweep closing what an adapter left open without failing a host that succeeded, work reported on a later agent-initiated turn, and a backgrounded shell command staying a tool call |
| `requests.spec.ts` | A declared request form: pick, note, and multi-select reaching the agent; an answer outside the offered list; declining while a required field is empty; prefill; dismissal saying the agent still waits; keyboard-only answering |
| `workspace-shell.spec.ts` | Pane toggles and persistence, palette including grouping and a cursor that skips headings, search including a hit's match snippet, focus restoration, settings round-trip, Artifacts tree, history browser, dialog-free creation |
| `file-viewer.spec.ts` | Tree/viewer swap preserving expansion, fullscreen and focus return, dialog sizing to content vs filling for a page, single-modal invariant, CSV table, sandboxed HTML refusing script and cookie access, image zoom, axe over both scales |
| `streaming.spec.ts` | Mid-turn painting against a deliberately paced adapter |
| `workspace-picking.spec.ts` | The three "which directory" flows: filter focused on open, folder browsing filterable inside one focus trap, a typed path reaching an unlisted folder, Enter-opens vs `Mod+Enter`-chooses |
| `select-list.spec.ts` | The shared choice list itself: filter appearing only past the threshold, cursor opening on the current value, one checkmark per group, a refused write surfacing |
| `commands.spec.ts` | The slash menu after a completed turn, and a control command opening a picker rather than sending a message |
| `settings-config.spec.ts` | The Configuration panel reporting what *this* backend resolved, a missing path called out, and the panel offering no way to edit a path from the browser |
| `adopt.spec.ts` | The adopt picker: reachable from the palette, a blocked row shown with its reason and navigating rather than re-adopting, an empty listing distinguished from a failed one, a refusal rendered, the combobox pattern, and a chosen row asking to follow the store |
| `file-operations.spec.ts` | Folder creation and its in-place errors, deletes that ask first, uploads including drop and overwrite, context menus, the raw route, and axe over both dialogs |
| `settings-identity-i18n.spec.ts` | Identity reporting when no `SOUL.md` resolves, language switching repainting and retagging (composer, skip link, file pane accessible names), a shortcut recorder refusing combos the browser keeps, and no *default* binding being one the browser would swallow |
| `accessibility.spec.ts` | Skip link, focus trapping and restoration including a picker's own field not becoming the node focus returns to, axe-core audit of every surface including both start-screen states and every overlay's list open |
| `mobile.spec.ts` | Phone-only: composer fits the viewport, sidebars out of flow, drawer and sheet behaviour, document cannot scroll, PWA installability, dark mode before paint, a long catalogue as a filterable sheet, a request form answerable by tapping, a three-deep delegation tree still fitting, the shortcut overlays' fields at or above 16px |

#### Why a second project exists

A layout bug below a breakpoint is observable only below that breakpoint, so the phone
viewport is the instrument that sees it rather than an optional variant.

`devices["Pixel 7"]`, because only Chromium is installed here and an iPhone profile runs
in Chromium anyway — claiming iOS coverage it does not have. A Chromium device profile
emulates what actually runs: `pointer: coarse`, a 412px viewport, and a device pixel ratio
above 1. **iOS Safari is therefore untested**, specifically `100dvh` under a collapsing
toolbar and zoom-on-small-input; a real device still wants a pass. The mobile specs assert
the consequences a Chromium phone can observe — nothing overflows the viewport, the
document has nothing to scroll, `window.scrollY` stays 0 after a 4000px wheel.

**These specs assert invariants, not pixels.** `overflowingElements()` reports every
element whose box extends past the right edge, which is the defect itself and stays
actionable across padding changes.


**The E2E web server is a two-process stack.** `e2e/start-backend-envagent.mjs` starts
`sylph-server` and a separately compiled `sylph-envagent`, obtains a short-lived pairing
code through the authenticated API, exchanges it with `sylph-envagent pair`, and then runs
the envagent against the adjacent loopback ingress. Browser fixtures select the connected
environment whose inventory reports the `e2e` adapter. This crosses the same
`Client → Server → EnvBridge → Envagent → Adapter` boundary as a single-machine install.
Starting only the backend would not exercise a shorter path — there is none — it would
leave every spec facing a server with no environment paired, which is why the harness owns
both processes rather than treating the envagent as optional.
Both children inherit the suite's throwaway roots, and the wrapper terminates them together
when Playwright stops the web server.

#### Startup readiness

The E2E launcher reports readiness only after its envagent is paired, connected, and
advertising the fixture adapter. Playwright waits for that stdout marker; HTTP health
alone describes the server and cannot establish that an environment is ready. The launcher
checks its ports before starting so the harness cannot reuse another process's server.
Tests holding intercepted requests release and await their handlers before teardown, so a
late routing error cannot escape into a later test. This suite blocks service-worker
registration, following Playwright's request-interception guidance, so page routes and
response assertions observe the requests they own. It does not establish offline-worker
behavior; that requires a separate context with workers enabled.

#### Fixture state

**`npm run e2e` wipes `.e2e-home/` before the run, and the npm script is the only place
that can.** Sidebar grouping is by workdir, so every spec's `mkdtemp` directory becomes a
permanent group in an uncleared journal; the suite's runtime then grows without bound and
specs start failing on their deadline in a rotating set, which reads as flakiness.
`SYLPH_E2E_KEEP_HOME=1` opts out to inspect a journal after a failure.

**Neither Playwright hook can do that wipe.** A `globalSetup` runs *after* `webServer`,
since `webServer` is a plugin and plugin setup is ordered first, and module scope in
`playwright.config.ts` re-runs in every worker process. Both delete `journal.db` under a
backend that has already opened it, and the suite answers `unable to open database file`.
`e2e/reset-home.mjs` runs from the npm script instead: one process, once, before Playwright
starts anything. A fresh home starts clean, so a first-run database error names this
hazard.

**The wipe removes the trigger, not the hazard.** One run creates over a hundred sessions
and the sidebar caps its listing at 200, so a spec late in the order can meet the cap —
past which a new session displaces an older row instead of adding one, and a "the group
grew by one" assertion stays flat. Assert on identity, matched by id, or count against a
`mkdtemp` directory of the spec's own.

**Clear the suite's ports before diagnosing a failure that happened before any spec ran.**
A backend surviving on 4327 from an interrupted run makes the suite fail at startup,
because `reuseExistingServer: false` refuses to trust it.

```sh
for p in 4327 4328 3101 3199; do lsof -ti tcp:$p | xargs kill 2>/dev/null; done
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

Four properties of the fixture are load-bearing, because each is the only way a suite can
observe the property beside it.

- **`paced` spaces its stages a second apart**, so streaming is distinguishable from
  batching.
- **`fanout` is wide and genuinely slow** — six delegates with real sleeps, ~2.6s total.
  Width is what the delegation overview's ranking needs, and duration is what gives each bar
  a distinct length: every other scenario emits its whole tree inside one millisecond, under
  a single `createdAtMs`.
- **It declares two select options in the same `model` category**, one with two choices and
  one with a long `CATALOGUE`. Below the filter threshold a config option is a `menu` of
  `menuitemradio`s and above it a combobox over a `listbox` — a different ARIA contract, and
  both land on the composer's control row together.
- **`set_config` refuses one value on purpose**, keeping the rejected-write path covered.

**Shortcut specs wait for a client-rendered control first.** The listener is attached by an
effect, so a key pressed immediately after `page.goto` can land on static markup — a real
race on a cold load.

#### Writing a spec that fails for one reason

Five rules, each of which a green suite cannot enforce.

**Name matching is a case-insensitive substring**, so `getByRole({ name: "Explore" })`
also matches "Focus Explore". Distinct names need `exact: true`. The costly form is a
name that is a substring of a *fallback* name, since the collision exists only in the
states that render a nameless node — the spec passes until it runs in a state that is
never the one under test.

**Assert on a container's own text, not its subtree.** `toContainText` walks descendants,
so a negative assertion on a card is coupled to copy anywhere beneath it and can be
satisfied for the wrong reason.

**Wait on a control whose presence changes, not on a disabled state.** A disabled
attribute usually encodes more than one fact — `Send` is disabled while a turn runs *and*
whenever the composer is empty, which is the state a spec is in right after sending. The
composer swaps Send for Stop while a turn is live, and `Stop` disappearing is a
settled-turn signal (`docs/database-schema.md` § A terminal status means the record is
finished).

**Use an auto-retrying assertion where an element mounts asynchronously.** A one-shot
`isVisible()` inside an `if` converts a lost race into a silently skipped step, and the
failure then surfaces in whatever the caller asserted next. An `if` is right only where
absence is a valid expected state.

**Two browser contexts, not two tabs**, for anything reaching a *second* client. Tabs
share a cookie jar.

#### Reading a failure

**An OOM kill reports zero failures.** Exit 137 or 143, no summary line, every spec green
up to the cut — indistinguishable from an interrupt. Check free pages first, and run in
groups rather than raising a timeout.

**Rotating failures point at a shared dependency, not at the specs.** A signal carrying
two facts under one name cannot be waited on, because a waiter cannot say which it got;
the symptom is a different spec failing each run. Look at what the failing set shares.

## Development Workflow

`docs/development.md` owns the commands for running the stack. `package.json` is the
authority on dependencies: four at runtime — `next`, `react`, `react-dom`,
`@tanstack/react-query` — plus the toolchain.

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
npm run e2e                         # Playwright, starts and pairs its own two-process stack
```

`npm run build` emits `output: "standalone"`. That bundle omits `.next/static` and
`public`, so an install copies both — otherwise the server starts cleanly and every asset
404s. `scripts/install.sh` does it and fails the install if `static` is missing. The backend
does not serve the frontend; there is no `dist/` to hand it.

Two stale-state traps live in `.next`, which is gitignored and so invisible to
`git status`. Deleting a route leaves `.next/dev/types/` behind, and the stale copy fails
the type check naming a module in `app/` that no source file references. A compiler reading
a half-written file reports line numbers that do not match the source. `rm -rf .next` and
re-run before believing either.

## What is absent

**Session management** — delete, move, and export have no endpoint behind them. Rename has
one (`PATCH /api/v1/sessions/{id}` accepts `title`) but no menu item calls it. Forking at a
turn would need an adapter able to resume from a prefix, which the session protocol does
not ask for.

**Search highlighting** — `Mod+F` filters turns rather than highlighting matches inside
them. Highlighting means reaching into the Markdown, diff, and code renderers to rewrite
their text nodes, which is why it is not a small change.

**Collaboration** — sharing, comments, and session-to-session diff are out of scope; the
Milestone 3 account and grant model provides the boundary for a future sharing surface.

## Decision Log

Two decisions shape the client beyond what § Technology Stack records, and both carry a
reversal condition.

### Primitives are built here rather than taken from a component library

Radix is the better answer on accessibility defaults and edge-case coverage, at several
times the first-load headroom. So the primitives live here: `lib/use-dialog.ts` (focus
trap, Escape, focus restoration), `components/overlay.tsx` (the modal frame),
`components/ui/menu.tsx` (arrow navigation, viewport clamping, outside-dismiss, and the
mobile sheet fork), `components/ui/sheet.tsx` (drag-to-dismiss), native `<details>`, and
the combobox pattern in the two pickers. The axe audit and the focus tests are what hold
the decision honest.

Reverse it when the budget rises, or when a primitive with real complexity is needed — a
date picker, a drag-and-drop tree, a virtualized listbox. Reverse it wholesale: one
pattern throughout beats two.

### Client state is two contexts and one external store

Shell state that must outlive navigation goes in context; preferences go in a
`useSyncExternalStore` over `localStorage`. That covers every case, for a store holding
about a dozen fields. `useSyncExternalStore` rather than state plus an effect, because it
avoids a cascading render on mount, keeps the server snapshot separate so hydration cannot
mismatch, and gets cross-tab sync from the `storage` event.

Reverse it when selector-scoped re-renders start showing up in the render-cost budget.

## References

- What the interface is: `docs/ui-layout.md`
- Endpoint contracts: `docs/api/`
- The event vocabulary an adapter emits: `docs/session-protocol.md`
- Tables, indexes, ancestry: `docs/database-schema.md`
- What is built and what is next: `docs/roadmap.md`

## Conversation reading model

[unified-trace-ui.md](unified-trace-ui.md) defines the replacement shared reader.
[unified-trace-storage.md](unified-trace-storage.md#snapshot-pages-and-live-changes)
defines reusable view leases, scoped cursors, immutable body revisions and coherent
change batches. Hidden readers release subscriptions; body and metadata budgets remain
independent, and stable object anchors survive revision/generation replacement.
